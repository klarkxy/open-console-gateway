<template>
  <n-modal
    :show="show"
    preset="card"
    :title="t('编辑连接')"
    class="destination-edit-modal"
    style="width: 720px; max-width: calc(100vw - 32px)"
    :mask-closable="false"
    :close-on-esc="!saving"
    @update:show="onOuterUpdateShow"
  >
    <div v-if="destination && draft" class="destination-edit-body">
      <template v-if="!grantStep">
        <n-form label-placement="top" class="destination-edit-form">
          <n-form-item :label="t('启用')">
            <n-switch v-model:value="draft.enabled" :disabled="saving" :aria-label="t('启用')" />
          </n-form-item>
          <n-form-item :label="t('名称')">
            <n-input
              v-model:value="draft.name"
              :disabled="saving"
              :input-props="{ 'aria-label': t('名称') }"
            />
          </n-form-item>
          <n-form-item :label="t('API 地址')" class="full-width-field">
            <n-input
              v-model:value="draft.endpoint_url"
              :disabled="saving"
              :placeholder="t('推荐填写不带 /v1 的 API 根地址；OCG 会自动补全 /v1 和协议路径。已带 /v1 时不会重复添加。')"
              :input-props="{ 'aria-label': t('API 地址') }"
              @update:value="onDefaultRouteFieldChange"
            />
          </n-form-item>
          <n-form-item :label="t('鉴权方式')">
            <n-select
              v-model:value="draft.auth_scheme"
              :options="authOptions"
              :disabled="saving"
              :aria-label="t('鉴权方式')"
              @update:value="onDefaultRouteFieldChange"
            />
          </n-form-item>
          <n-form-item :label="t('上游协议')">
            <n-select
              v-model:value="draft.upstream_protocol"
              :options="protocolOptions"
              :disabled="saving"
              :aria-label="t('上游协议')"
              @update:value="onDefaultRouteFieldChange"
            />
          </n-form-item>
          <n-form-item v-if="presetWithRoutes" class="full-width-field">
            <div class="protocol-preset-actions">
              <n-button
                attr-type="button"
                size="small"
                secondary
                :disabled="saving"
                @click="applyPresetRoutes"
              >
                {{ t("采用预设协议") }}
              </n-button>
              <a
                :href="presetWithRoutes.docsUrl"
                target="_blank"
                rel="noopener noreferrer"
              >{{ t("官方文档") }}</a>
            </div>
          </n-form-item>
          <n-form-item
            v-if="extraProtocolRoutes.length > 0 || canAddProtocolRoute"
            :label="t('额外协议')"
            class="full-width-field"
          >
            <div class="protocol-route-rows">
              <div
                v-for="row in extraProtocolRoutes"
                :key="row.index"
                class="protocol-route-row"
              >
                <n-select
                  v-model:value="draft.protocol_routes[row.index].protocol"
                  :options="protocolOptionsFor(row.index)"
                  :disabled="saving"
                  :aria-label="t('上游协议')"
                />
                <n-select
                  v-model:value="draft.protocol_routes[row.index].auth_scheme"
                  :options="authOptions"
                  :disabled="saving"
                  :aria-label="t('鉴权方式')"
                />
                <n-input
                  v-model:value="draft.protocol_routes[row.index].endpoint_url"
                  :disabled="saving"
                  :placeholder="t('协议地址')"
                  :input-props="{ 'aria-label': t('协议地址') }"
                />
                <n-button
                  attr-type="button"
                  quaternary
                  :disabled="saving"
                  @click="removeProtocolRoute(row.index)"
                >
                  {{ t("删除") }}
                </n-button>
              </div>
              <n-button
                v-if="canAddProtocolRoute"
                attr-type="button"
                size="small"
                secondary
                :disabled="saving"
                @click="addProtocolRoute"
              >
                {{ t("添加协议") }}
              </n-button>
            </div>
          </n-form-item>
          <n-form-item :label="t('模型映射')" class="full-width-field">
            <div class="mapping-rows">
              <div v-for="(row, index) in draft.models" :key="index" class="mapping-row">
                <div class="mapping-row-main">
                  <n-switch v-model:value="row.enabled" :disabled="saving" :aria-label="`${t('模型')} ${row.public_model} ${t('启用')}`" />
                  <n-input
                    v-model:value="row.public_model"
                    :disabled="saving"
                    :placeholder="t('对外模型名')"
                    :input-props="{ 'aria-label': t('对外模型名') }"
                  />
                  <n-input
                    v-model:value="row.upstream_model"
                    :disabled="saving"
                    :placeholder="t('上游模型 ID')"
                    :input-props="{ 'aria-label': t('上游模型 ID') }"
                  />
                  <n-button
                    attr-type="button"
                    quaternary
                    :disabled="draft.models.length < 2 || saving"
                    @click="removeModel(index)"
                  >
                    {{ t("删除映射") }}
                  </n-button>
                </div>
                <div class="mapping-row-route">
                  <n-select
                    :value="row.upstream_override ? 'override' : 'inherit'"
                    :options="routeModeOptions"
                    :disabled="saving"
                    :aria-label="t('上游连接')"
                    @update:value="(mode) => setRowRouteMode(row, String(mode))"
                  />
                  <template v-if="row.upstream_override">
                    <n-select
                      v-model:value="row.upstream_override.protocol"
                      :options="protocolOptions"
                      :disabled="saving"
                      :aria-label="t('覆盖的上游协议')"
                    />
                    <n-input
                      v-model:value="row.upstream_override.endpoint_url"
                      :disabled="saving"
                      :placeholder="t('覆盖的上游地址（必填）')"
                      :input-props="{ 'aria-label': t('覆盖的上游地址') }"
                    />
                  </template>
                </div>
              </div>
              <n-button attr-type="button" size="small" secondary :disabled="saving" @click="addModel">
                {{ t("添加模型") }}
              </n-button>
            </div>
          </n-form-item>
        </n-form>
        <n-alert v-if="formErrorText" type="error" :title="formErrorText" class="destination-edit-error" />
      </template>

      <section v-else class="grant-step" :aria-label="t('受影响的 Key')">
        <h3 class="grant-step__title">{{ t("受影响的 Key") }}</h3>
        <p class="grant-step__note">{{ t("保存前选择允许接收新地址授权的 Key；未选择的 Key 对新地址的请求将失败。") }}</p>
        <ul class="grant-step__list">
          <li v-for="candidate in grantCandidates" :key="candidate.id" class="grant-step__item">
            <n-checkbox
              :checked="checkedGrantIds.has(candidate.id)"
              :disabled="saving"
              :aria-label="candidate.name"
              @update:checked="(checked: boolean) => toggleGrant(candidate.id, checked)"
            >
              {{ candidate.name }}
            </n-checkbox>
            <n-tag v-if="!candidate.enabled" size="small" :bordered="false">{{ t("已禁用") }}</n-tag>
            <n-tag
              size="small"
              :type="candidate.covered ? 'default' : 'warning'"
              :bordered="false"
            >{{ candidate.covered ? t("已覆盖新地址") : t("需要新授权") }}</n-tag>
          </li>
        </ul>
      </section>
    </div>

    <template #footer>
      <div class="destination-edit-footer">
        <template v-if="grantStep">
          <n-button secondary :disabled="saving" @click="grantStep = false">
            {{ t("返回") }}
          </n-button>
          <n-button type="primary" :loading="saving" @click="confirmGrantSave">
            {{ t("确认并保存") }}
          </n-button>
        </template>
        <template v-else>
          <n-button secondary :disabled="saving" @click="emit('update:show', false)">
            {{ t("取消") }}
          </n-button>
          <n-button type="primary" :loading="saving" @click="onSave">
            {{ t("保存") }}
          </n-button>
        </template>
      </div>
    </template>
  </n-modal>
</template>

<script setup lang="ts">
import { computed, ref, toRef, watch } from "vue";
import {
  NAlert,
  NButton,
  NCheckbox,
  NForm,
  NFormItem,
  NSwitch,
  NInput,
  NModal,
  NSelect,
  NTag,
  useMessage,
} from "naive-ui";
import type {
  Destination,
  DestinationCredential,
  DestinationPatchInput,
} from "../api/destinations.ts";
import { DashboardRequestError, isRevisionConflict } from "../api/dashboard.ts";
import { useDestinationsStore } from "../stores/destinations.ts";
import { t, type MessageKey } from "../i18n/index.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";
import { useLocalizedModalCloseLabel } from "../utils/modal-close-label.ts";
import {
  DESTINATION_EDIT_ISSUE_KEYS,
  addDraftProtocolRoute,
  applyPresetProtocolRoutesToDraft,
  destinationEditDraft,
  removeDraftProtocolRoute,
  syncDraftDefaultRoute,
  unusedDraftProtocol,
  withAuthorizedCredentials,
  type DestinationEditDraft,
  type DestinationGrantCandidate,
  type DestinationModelDraft,
} from "../domain/destination-edit.ts";
import { planDestinationSave } from "../domain/destination-edit-save.ts";
import { PROVIDER_PROTOCOLS, protocolDisplayName } from "../domain/provider-contracts.ts";
import { PROVIDER_PRESETS } from "../domain/provider-presets.ts";
import type { MutationExpectation } from "../api/generated/dashboard-v3.ts";
import type { ConnectionEndpoint } from "../api/connections.ts";
import type { ProtocolDto } from "../api/destinations.ts";

const props = defineProps<{
  show: boolean;
  /** Editable configurable HTTP destination; null while the modal is closed. */
  destination: Destination | null;
  /** Credential projection used to compute grant consent candidates. */
  credentials: readonly DestinationCredential[];
  endpoints: readonly ConnectionEndpoint[];
  /** Persisted preset id from the parent definition; never inferred from names. */
  presetId?: string | null;
}>();

const emit = defineEmits<{
  (event: "update:show", value: boolean): void;
  (event: "saved", destinationId: string): void;
}>();

useLocalizedModalCloseLabel(toRef(props, "show"), "destination-edit-modal");
const message = useMessage();
const destinationsStore = useDestinationsStore();

const draft = ref<DestinationEditDraft | null>(null);
const formErrorKey = ref<MessageKey | null>(null);
const saving = ref(false);
/** The grant-consent sub-step, shown only for route-changing saves with Keys. */
const grantStep = ref(false);
const grantCandidates = ref<readonly DestinationGrantCandidate[]>([]);
/** Explicit user selection only; nothing is preselected or auto-authorized. */
const checkedGrantIds = ref<ReadonlySet<string>>(new Set());
const pendingInput = ref<DestinationPatchInput | null>(null);
/** CAS pair belonging to the row copied into `draft`, not a later store load. */
const capturedExpectation = ref<MutationExpectation | null>(null);

const formErrorText = computed(() => (formErrorKey.value ? t(formErrorKey.value) : ""));
const protocolOptions = computed(() => PROVIDER_PROTOCOLS.map((value) => ({
  value,
  label: protocolDisplayName(value),
})));
const authOptions = computed(() => ([
  { value: "bearer", label: "Bearer" },
  { value: "x_api_key", label: "x-api-key" },
  { value: "none", label: t("无鉴权") },
]));
const routeModeOptions = computed(() => [
  { value: "inherit", label: t("跟随连接默认") },
  { value: "override", label: t("覆盖上游地址") },
]);

const presetWithRoutes = computed(() => {
  const presetId = props.presetId?.trim();
  if (!presetId) return null;
  const preset = PROVIDER_PRESETS.find((entry) => entry.id === presetId) ?? null;
  return preset?.protocolRoutes && preset.protocolRoutes.length > 0 ? preset : null;
});

const extraProtocolRoutes = computed(() => (
  (draft.value?.protocol_routes ?? []).slice(1).map((route, offset) => ({
    index: offset + 1,
    route,
  }))
));

const canAddProtocolRoute = computed(() => (
  Boolean(draft.value && unusedDraftProtocol(draft.value))
));

function onDefaultRouteFieldChange(): void {
  if (!draft.value || saving.value) return;
  syncDraftDefaultRoute(draft.value);
}

function addProtocolRoute(): void {
  if (!draft.value || saving.value) return;
  addDraftProtocolRoute(draft.value);
}

function removeProtocolRoute(index: number): void {
  if (!draft.value || saving.value) return;
  removeDraftProtocolRoute(draft.value, index);
}

function applyPresetRoutes(): void {
  if (!draft.value || saving.value || !presetWithRoutes.value) return;
  applyPresetProtocolRoutesToDraft(draft.value, presetWithRoutes.value);
}

function protocolOptionsFor(index: number) {
  const current = draft.value?.protocol_routes[index]?.protocol ?? "";
  const used = new Set(
    (draft.value?.protocol_routes ?? [])
      .map((route, routeIndex) => (routeIndex === index ? "" : route.protocol)),
  );
  return protocolOptions.value.filter((option) => (
    option.value === current || !used.has(option.value as ProtocolDto)
  ));
}

function resetDraft(destination: Destination): void {
  draft.value = destinationEditDraft(destination);
  capturedExpectation.value = destinationsStore.expectation
    ? { ...destinationsStore.expectation }
    : null;
  formErrorKey.value = null;
  grantStep.value = false;
  grantCandidates.value = [];
  checkedGrantIds.value = new Set();
  pendingInput.value = null;
}

watch(
  () => [props.show, props.destination?.id ?? null] as const,
  (value, previous) => {
    const [visible, destinationId] = value;
    if (!visible || !destinationId || !props.destination) return;
    // A fresh open starts from the persisted row; conflict reloads re-init below.
    if (!(previous?.[0] ?? false)) resetDraft(props.destination);
  },
  { immediate: true },
);

function addModel(): void {
  if (!draft.value || saving.value) return;
  const protocol = PROVIDER_PROTOCOLS.includes(draft.value.upstream_protocol as ProtocolDto)
    ? draft.value.upstream_protocol as ProtocolDto
    : undefined;
  draft.value.models.push({
    enabled: true,
    public_model: "",
    upstream_model: "",
    protocols: protocol ? [protocol] : [],
    preferred: protocol ?? null,
    upstream_override: null,
  });
}

function removeModel(index: number): void {
  if (!draft.value || saving.value || draft.value.models.length < 2) return;
  draft.value.models.splice(index, 1);
}

function setRowRouteMode(row: DestinationModelDraft, mode: string): void {
  if (!draft.value || saving.value) return;
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

function toggleGrant(id: string, checked: boolean): void {
  if (saving.value) return;
  const next = new Set(checkedGrantIds.value);
  if (checked) next.add(id);
  else next.delete(id);
  checkedGrantIds.value = next;
}

function onSave(): void {
  const destination = props.destination;
  if (!destination || !draft.value || saving.value) return;
  formErrorKey.value = null;
  const plan = planDestinationSave(destination, props.credentials, draft.value, props.endpoints);
  if (plan.status === "invalid") {
    formErrorKey.value = DESTINATION_EDIT_ISSUE_KEYS[plan.issue];
    return;
  }
  if (plan.status === "grant_consent") {
    pendingInput.value = plan.input;
    grantCandidates.value = plan.candidates;
    checkedGrantIds.value = new Set();
    grantStep.value = true;
    return;
  }
  void persist(plan.input, []);
}

function confirmGrantSave(): void {
  const input = pendingInput.value;
  if (!input || saving.value) return;
  void persist(input, [...checkedGrantIds.value]);
}

async function persist(input: DestinationPatchInput, authorizeIds: readonly string[]): Promise<void> {
  const destination = props.destination;
  if (!destination || saving.value) return;
  saving.value = true;
  try {
    await destinationsStore.patchDestination(
      destination.id,
      withAuthorizedCredentials(input, authorizeIds),
      capturedExpectation.value ?? undefined,
    );
    message.success(t("连接已保存"));
    emit("saved", destination.id);
    emit("update:show", false);
  } catch (error) {
    if (error instanceof DashboardRequestError && isRevisionConflict(error)) {
      // The store already reloaded the projection; edit against the fresh row.
      const fresh = destinationsStore.byId.get(destination.id) ?? null;
      if (fresh) resetDraft(fresh);
      message.warning(t("状态已变化，请刷新后重试。"));
      return;
    }
    message.error(t("保存失败：{error}", { error: dashboardErrorDetail(error) }));
  } finally {
    saving.value = false;
  }
}

function onOuterUpdateShow(value: boolean): void {
  if (value) {
    emit("update:show", true);
    return;
  }
  // In-flight saves and failed validations keep the draft open.
  if (saving.value) return;
  emit("update:show", false);
}
</script>

<style scoped>
.destination-edit-body {
  max-height: min(560px, calc(100vh - 220px));
  overflow: auto;
}
.destination-edit-form {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  column-gap: var(--ocg-space-lg);
}
.full-width-field {
  grid-column: 1 / -1;
}
.destination-edit-error {
  margin-top: var(--ocg-space-md);
}
.mapping-rows {
  display: grid;
  gap: var(--ocg-space-sm);
  width: 100%;
}
.mapping-row {
  display: grid;
  gap: var(--ocg-space-xs);
  padding: var(--ocg-space-sm) var(--ocg-space-md);
  border: 1px solid var(--ocg-border);
  border-radius: var(--ocg-radius-md);
  background: var(--ocg-canvas);
}
.mapping-row-main {
  display: grid;
  grid-template-columns: auto minmax(0, 1fr) minmax(0, 1fr) auto;
  gap: var(--ocg-space-sm);
  align-items: center;
}
.mapping-row-route {
  display: grid;
  grid-template-columns: minmax(140px, auto) minmax(140px, auto) minmax(0, 1fr);
  gap: var(--ocg-space-sm);
  align-items: center;
}
.protocol-preset-actions {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--ocg-space-sm);
}
.protocol-route-rows {
  display: grid;
  gap: var(--ocg-space-sm);
  width: 100%;
}
.protocol-route-row {
  display: grid;
  grid-template-columns: minmax(140px, auto) minmax(140px, auto) minmax(0, 1fr) auto;
  gap: var(--ocg-space-sm);
  align-items: center;
}
.grant-step__title {
  margin: 0 0 var(--ocg-space-xs);
  color: var(--ocg-ink);
  font-size: var(--ocg-font-md);
  font-weight: 600;
}
.grant-step__note {
  margin: 0 0 var(--ocg-space-md);
  color: var(--ocg-muted);
  font-size: var(--ocg-font-sm);
}
.grant-step__list {
  display: grid;
  gap: var(--ocg-space-sm);
  margin: 0;
  padding: 0;
  list-style: none;
}
.grant-step__item {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--ocg-space-sm);
  padding: var(--ocg-space-sm) var(--ocg-space-md);
  border: 1px solid var(--ocg-border);
  border-radius: var(--ocg-radius-md);
  background: var(--ocg-canvas);
}
.destination-edit-footer {
  display: flex;
  justify-content: flex-end;
  gap: var(--ocg-space-sm);
}

@media (max-width: 640px) {
  .destination-edit-form {
    grid-template-columns: minmax(0, 1fr);
  }
  .mapping-row-route {
    grid-template-columns: minmax(0, 1fr);
  }
  .protocol-route-row {
    grid-template-columns: minmax(0, 1fr);
  }
}
</style>
