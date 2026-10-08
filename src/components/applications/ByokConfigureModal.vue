<template>
  <n-modal
    :show="show"
    preset="card"
    :title="isUpdate ? t('更新 {client} 配置', { client: label }) : t('配置 {client}', { client: label })"
    class="byok-configure-modal"
    style="width: 560px; max-width: calc(100vw - 32px)"
    :mask-closable="false"
    :close-on-esc="!mutating"
    @update:show="setVisible"
  >
    <div v-if="view" class="byok-confirm">
      <n-alert
        v-if="actionError"
        type="error"
        :title="actionError"
        closable
        @close="actionError = ''"
      />

      <p v-if="preparing" role="status" class="byok-hint">{{ t("正在准备更新预览…") }}</p>
      <n-button v-if="!preparing && !snapshotMatches" secondary @click="prepare()">{{ t("重试") }}</n-button>
      <div>
        <p class="byok-paths-title">{{ t("目标配置文件") }}</p>
        <code class="byok-selected-path">{{ displayTarget || t("未知") }}</code>
      </div>

      <div>
        <p class="byok-paths-title">{{ t("API 地址") }}</p>
        <code class="byok-selected-path" data-review="gateway">{{ view.gatewayV1Url }}</code>
      </div>
      <p class="byok-hint">
        {{ t("将使用 Key {name} 及其全部已发布模型。", { name: keyName }) }}
      </p>
      <p class="byok-hint">
        {{ t("保存会把 Key 明文写入此文件；请确认本机访问权限。首次写入前会创建备份，中断后可在此页恢复。") }}
      </p>
      <p v-if="client === 'minimax'" class="byok-hint">
        {{ t("MiniMax 配置会重新排版；原有注释保留在首次备份中。") }}
      </p>

      <div v-if="client === 'copilot'" class="copilot-budget">
        <p class="byok-hint">{{ t("预算用于 VS Code 上下文管理，受已知模型上限约束，不修改 OCG 元数据。实际请求参数由客户端决定。每次更新时可调整。") }}</p>
        <p class="byok-hint">{{ t("未调整预算时保留客户端的逐模型设置；调整后应用到全部模型。") }}</p>
        <label :for="`copilot-input-${client}`">{{ t("输入 Token 预算") }}</label>
        <n-input-number :input-props="{ id: `copilot-input-${client}`, 'aria-label': t('输入 Token 预算') }" v-model:value="inputBudget" :min="1" :max="4294967295" :precision="0" :disabled="mutating" />
        <label :for="`copilot-output-${client}`">{{ t("输出 Token 预算") }}</label>
        <n-input-number :input-props="{ id: `copilot-output-${client}`, 'aria-label': t('输出 Token 预算') }" v-model:value="outputBudget" :min="1" :max="4294967295" :precision="0" :disabled="mutating" />
      </div>

      <div v-if="preview && prepared" class="byok-plan" aria-live="polite">
        <div v-for="delta in deltas" :key="delta.kind" :data-delta="delta.kind">
          <p class="byok-paths-title">{{ t(BYOK_DELTA_KEYS[delta.kind]) }} ({{ delta.ids.length }})</p>
          <ul v-if="delta.ids.length" class="byok-model-list"><li v-for="id in delta.ids" :key="id"><code>{{ id }}</code></li></ul>
        </div>
        <p v-if="preview.previousDefaultModelId !== preview.defaultModelId" class="byok-hint" data-delta="default">
          {{ t("默认模型变化") }}: <code>{{ preview.previousDefaultModelId ?? t("未设置") }}</code> → <code>{{ preview.defaultModelId ?? t("未设置") }}</code>
        </p>
        <n-checkbox v-if="preview.requiresTakeover" v-model:checked="takeoverAck" data-ack="takeover" :disabled="busy">{{ t("我同意接管同名配置；撤销接管时会恢复接管前的字段。") }}</n-checkbox>
        <n-checkbox v-if="preview.requiresOverwrite" v-model:checked="overwriteAck" data-ack="overwrite" :disabled="busy">{{ t("我同意覆盖 OCG 托管字段中的外部改动。") }}</n-checkbox>
        <template v-if="preview.removedModelsWithCustomizations.length">
          <n-checkbox v-model:checked="removalAck" data-ack="removal" :disabled="busy">{{ t("我同意移除列出的模型及其不再使用的供应商，包括自定义字段。") }}</n-checkbox>
          <ul class="byok-model-list"><li v-for="id in preview.removedModelsWithCustomizations" :key="id"><code>{{ id }}</code></li></ul>
        </template>
      </div>
      <n-checkbox v-if="view.requiresClosedClient" data-ack="closed" v-model:checked="closedAck" :disabled="busy">
        {{ t("我已完全关闭 {client}（CLI 与桌面应用），确认没有正在运行的实例。", { client: label }) }}
      </n-checkbox>
    </div>
    <template #footer>
      <div class="byok-confirm-footer">
        <n-button quaternary :disabled="mutating" @click="setVisible(false)">
          {{ t("取消") }}
        </n-button>
        <n-button
          type="primary"
          :loading="busy"
          :disabled="!confirmable"
          @click="confirm"
        >
          {{ mutating ? t("保存中…") : t("确认保存") }}
        </n-button>
      </div>
    </template>
  </n-modal>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, ref, toRef, watch } from "vue";
import { NAlert, NButton, NCheckbox, NInputNumber, NModal } from "naive-ui";
import { DashboardRequestError, isRevisionConflict } from "../../api/dashboard.ts";
import type { ByokApplicationView } from "../../api/byok-applications.ts";
import { t } from "../../i18n/index.ts";
import { useConnectionStore } from "../../stores/connection.ts";
import { useControlPlaneStore } from "../../stores/controlPlane.ts";
import { useByokApplicationsStore } from "../../stores/byokApplications.ts";
import { dashboardErrorDetail } from "../../utils/errors.ts";
import { useLocalizedModalCloseLabel } from "../../utils/modal-close-label.ts";
import {
  BYOK_CLIENT_LABELS, BYOK_DELTA_KEYS, COPILOT_DEFAULT_TOKEN_BUDGET,
  copilotTokenBudgetValid, HARNESS_DEFAULT_KEY_NAMES, byokDisplayTarget,
  byokMutationExpectation, byokReviewGate, refreshConnectionAfterHarnessMutation,
  type ByokClientId,
} from "../../domain/byok-applications.ts";

const props = defineProps<{ show: boolean; client: ByokClientId; view: ByokApplicationView | null; targetPath?: string }>();
const emit = defineEmits<{ "update:show": [show: boolean]; saved: [result: ByokApplicationView]; conflict: [] }>();
const connectionStore = useConnectionStore();
const controlPlane = useControlPlaneStore();
const store = useByokApplicationsStore();
const actionError = ref("");
const closedAck = ref(false);
const takeoverAck = ref(false);
const overwriteAck = ref(false);
const removalAck = ref(false);
const mutating = ref(false);
const preparing = ref(false);
// Only confirmation tokens are local; the preview itself belongs to the store.
const prepared = ref<{ fingerprint: string; plan: string; revision: number; generation: number; session: number } | null>(null);
const inputBudget = ref<number | null>(COPILOT_DEFAULT_TOKEN_BUDGET.maxInputTokens);
const outputBudget = ref<number | null>(COPILOT_DEFAULT_TOKEN_BUDGET.maxOutputTokens);
const budgetEdited = ref(false);
let initializingBudget = false;
let requestEpoch = 0;

const label = computed(() => BYOK_CLIENT_LABELS[props.client]);
const keyName = computed(() => HARNESS_DEFAULT_KEY_NAMES[props.client]);
const view = computed(() => store.peek(props.client, props.targetPath)?.view ?? null);
const preview = computed(() => view.value?.preview ?? null);
const busy = computed(() => mutating.value || preparing.value);
const isUpdate = computed(() => (view.value?.configuredModelIds.length ?? 0) > 0);
const displayTarget = computed(() => byokDisplayTarget(view.value));
const deltas = computed(() => [
  { kind: "added" as const, ids: preview.value?.addedModelIds ?? [] },
  { kind: "removed" as const, ids: preview.value?.removedModelIds ?? [] },
  { kind: "updated" as const, ids: preview.value?.updatedModelIds ?? [] },
]);
const snapshotMatches = computed(() => {
  const current = view.value;
  const tokens = prepared.value;
  return tokens !== null && tokens.session === connectionStore.currentSession()
    && current?.fingerprint === tokens.fingerprint
    && current?.preview?.planFingerprint === tokens.plan
    && current?.revision.revision === tokens.revision
    && current?.revision.processGeneration === tokens.generation;
});
const confirmable = computed(() => props.show && !busy.value && snapshotMatches.value
  && (props.client !== "copilot" || copilotTokenBudgetValid(inputBudget.value, outputBudget.value))
  && byokReviewGate(view.value, { closed: closedAck.value, takeover: takeoverAck.value, overwrite: overwriteAck.value, removal: removalAck.value }) === "ready");
function resetAcknowledgements(): void {
  closedAck.value = false; takeoverAck.value = false; overwriteAck.value = false; removalAck.value = false;
}
function budgetInput() {
  return props.client === "copilot" && budgetEdited.value
    ? { copilotTokenBudget: { maxInputTokens: inputBudget.value!, maxOutputTokens: outputBudget.value! } } : {};
}
async function prepare(): Promise<void> {
  const epoch = ++requestEpoch;
  prepared.value = null;
  resetAcknowledgements();
  if (!props.show || (props.client === "copilot" && !copilotTokenBudgetValid(inputBudget.value, outputBudget.value))) {
    preparing.value = false;
    store.discardPreview(props.client, props.targetPath);
    return;
  }
  const client = props.client;
  const target = props.targetPath;
  const session = connectionStore.currentSession();
  preparing.value = true;
  try {
    const committed = await store.preview(client, { targetPath: target ?? null, ...budgetInput() });
    if (epoch !== requestEpoch || !props.show || session !== connectionStore.currentSession() || !committed) return;
    const current = store.peek(client, target)?.view;
    if (current?.fingerprint && current.preview?.planFingerprint) prepared.value = {
      fingerprint: current.fingerprint, plan: current.preview.planFingerprint,
      revision: current.revision.revision, generation: current.revision.processGeneration, session,
    };
    if (!budgetEdited.value && current?.copilotTokenBudget) {
      initializingBudget = true;
      inputBudget.value = current.copilotTokenBudget.maxInputTokens;
      outputBudget.value = current.copilotTokenBudget.maxOutputTokens;
      initializingBudget = false;
    }
  } catch (error) {
    if (epoch === requestEpoch && props.show && session === connectionStore.currentSession()) actionError.value = t("预览失败：{error}", { error: dashboardErrorDetail(error) });
  } finally { if (epoch === requestEpoch) preparing.value = false; }
}
watch([() => props.show, () => props.client, () => props.targetPath], ([visible], previous) => {
  requestEpoch += 1;
  prepared.value = null;
  preparing.value = false;
  if (!visible) { if (previous?.[0]) store.discardPreview(previous[1] ?? props.client, previous[2]); return; }
  actionError.value = "";
  initializingBudget = true;
  const saved = props.view?.copilotTokenBudget ?? COPILOT_DEFAULT_TOKEN_BUDGET;
  inputBudget.value = saved.maxInputTokens;
  outputBudget.value = saved.maxOutputTokens;
  budgetEdited.value = false;
  initializingBudget = false;
  void prepare();
}, { immediate: true });
watch([inputBudget, outputBudget], () => {
  if (!props.show || initializingBudget || mutating.value) return;
  budgetEdited.value = true;
  void prepare();
}, { flush: "sync" });
function setVisible(show: boolean): void {
  if (mutating.value) return;
  if (!show) { requestEpoch += 1; prepared.value = null; preparing.value = false; store.discardPreview(props.client, props.targetPath); }
  emit("update:show", show);
}
onBeforeUnmount(() => { requestEpoch += 1; if (props.show && !mutating.value) store.discardPreview(props.client, props.targetPath); });
async function confirm(): Promise<void> {
  const current = view.value;
  const tokens = prepared.value;
  if (!current || !tokens || !confirmable.value) return;
  const epoch = requestEpoch;
  const session = { captured: tokens.session, current: () => connectionStore.currentSession() };
  actionError.value = "";
  mutating.value = true;
  try {
    const result = await controlPlane.runMutation((expectation) => store.configure(props.client, {
      targetPath: props.targetPath ?? null, expectedFingerprint: tokens.fingerprint,
      previewFingerprint: tokens.plan, clientClosed: current.requiresClosedClient ? closedAck.value : false,
      acknowledgeTakeover: takeoverAck.value, acknowledgeOverwrite: overwriteAck.value, acknowledgeRemoval: removalAck.value,
      ...budgetInput(),
    }, expectation), byokMutationExpectation(current));
    if (session.captured !== session.current()) { emit("update:show", false); return; }
    if (epoch !== requestEpoch || !props.show) return;
    emit("saved", result);
  } catch (error) {
    if (session.captured !== session.current()) { emit("update:show", false); return; }
    if (epoch !== requestEpoch || !props.show) return;
    if (isRevisionConflict(error) || (error instanceof DashboardRequestError && error.status === 409)) {
      actionError.value = t("更新计划已变化，请重新检查预览并确认。");
      await prepare();
    } else {
      actionError.value = t("保存失败：{error}", { error: dashboardErrorDetail(error) });
      prepared.value = null;
      store.discardPreview(props.client, props.targetPath);
      await store.inspect(props.client, props.targetPath, { retain: true });
    }
  } finally {
    mutating.value = false;
    void refreshConnectionAfterHarnessMutation(() => connectionStore.reloadAfterMutation(session.captured), session);
  }
}
useLocalizedModalCloseLabel(toRef(props, "show"), "byok-configure-modal");
</script>

<style scoped>
.byok-confirm {
  display: grid;
  gap: var(--ocg-space-md);
}
.byok-paths-title {
  margin: 0 0 var(--ocg-space-xs);
  color: var(--ocg-ink);
  font-size: var(--ocg-font-sm);
  font-weight: 600;
}
.byok-selected-path {
  overflow-wrap: anywhere;
}
.byok-hint {
  margin: 0;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-sm);
  line-height: 1.6;
}
.copilot-budget {
  display: grid;
  gap: var(--ocg-space-xs);
}
.byok-plan { display: grid; gap: var(--ocg-space-md); }
.byok-model-list { margin: 0; padding-left: 20px; overflow-wrap: anywhere; }
.byok-confirm-footer {
  display: flex;
  justify-content: flex-end;
  gap: var(--ocg-space-sm);
}
</style>
