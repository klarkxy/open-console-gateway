<template>
  <section class="byok-section" :aria-labelledby="`byok-${client}-title`">
    <h2 :id="`byok-${client}-title`" class="sr-only">{{ label }}</h2>

    <div
      v-if="initialLoading"
      class="byok-state"
      role="status"
      aria-live="polite"
      :aria-label="t('加载中…')"
    >
      <n-spin size="small" />
    </div>

    <n-alert
      v-else-if="entry?.error && !view"
      type="error"
      :title="t('加载应用状态失败：{error}', { error: entry.error })"
    >
      <n-button size="small" secondary :loading="entry.loading" @click="load()">
        {{ t("重试") }}
      </n-button>
    </n-alert>

    <div v-else-if="view" class="byok-body">
      <div class="byok-toolbar">
        <n-button secondary size="small" :loading="entry?.loading" @click="load({ retain: true })">
          {{ t("刷新") }}
        </n-button>
      </div>
      <n-alert
        v-if="entry?.error"
        type="warning"
        :title="t('加载应用状态失败：{error}', { error: entry.error })"
      >
        <n-button size="small" secondary :loading="entry.loading" @click="load({ retain: true })">
          {{ t("重试") }}
        </n-button>
      </n-alert>
      <n-alert
        v-if="actionError"
        type="error"
        :title="actionError"
        closable
        @close="actionError = ''"
      />

      <div class="byok-status-row">
        <n-tag :type="presentation.tone" size="small">{{ t(presentation.labelKey) }}</n-tag>
        <span v-if="view.activationRequired" class="byok-activation">
          {{ t("已保存，待激活") }}
        </span>
      </div>
      <p class="byok-hint">{{ t(presentation.hintKey, { client: label }) }}</p>
      <p v-if="view.configureSupported" class="byok-hint">
        {{ t("将使用 Key {name} 及其全部已发布模型。", { name: keyName }) }}
      </p>
      <p v-if="client === 'copilot'" class="byok-hint">
        {{ t("用于 VS Code 聊天和 Agent。Agent 只显示已知支持工具调用的模型；行内自动补全使用独立模型。保存后重新打开 VS Code，在模型选择器中选择 Open Console Gateway。") }}
      </p>
      <p v-if="view.detail" class="byok-detail">{{ view.detail }}</p>

      <div class="byok-target">
        <label :for="`byok-target-${client}`" class="byok-paths-title">{{ t("目标配置文件") }}</label>
        <div class="byok-target-row">
          <n-input
            :input-props="{ id: `byok-target-${client}`, 'aria-label': t('目标配置文件') }"
            :value="targetDraft"
            :disabled="busy"
            :placeholder="view.configPath ?? ''"
            @update:value="targetDraft = $event"
          />
          <n-button secondary size="small" :loading="entry?.loading" :disabled="busy" @click="applyTarget">
            {{ t("检查") }}
          </n-button>
        </div>
        <p class="byok-hint">{{ t("留空使用自动检测的默认路径；也可填写自定义 Profile 的完整配置文件路径。") }}</p>
        <code v-if="displayTarget" class="byok-selected-path">{{ displayTarget }}</code>
        <p v-if="view.discoverySource" class="byok-hint">
          {{ t("发现来源：{source}", { source: view.discoverySource }) }}
        </p>
        <p v-if="view.backupPath" class="byok-hint">
          {{ t("首次写入前的备份：{path}", { path: view.backupPath }) }}
        </p>
      </div>

      <div v-if="view.configuredModelIds.length > 0" class="byok-configured">
        <p class="byok-paths-title">
          {{ t("已保存的 OCG 模型（{count}）", { count: view.configuredModelIds.length }) }}
        </p>
        <p v-if="client !== 'copilot'" class="byok-hint">
          {{ t("默认模型") }}: <code>{{ view.defaultModelId ?? t("未设置") }}</code>
        </p>
      </div>

      <div v-if="!view.configureSupported" class="byok-manual">
        <p class="byok-paths-title">{{ t("手动配置") }}</p>
        <p v-if="client === 'copilot'" class="byok-hint">{{ t("在 VS Code 中打开管理语言模型，添加 Custom Endpoint；填写每个模型的完整 API 地址、模型 ID 和客户端预算，并使用 OCG Key 鉴权。") }}</p>
        <p v-else class="byok-hint">{{ t("在 {client} 中添加 OpenAI 兼容供应商，使用以下网关地址：", { client: label }) }}</p>
        <code class="byok-selected-path">{{ view.gatewayV1Url }}</code>
        <p class="byok-hint">{{ t("使用控制台中任一已启用的 Key 作为 API Key；明文只保存在客户端本机。") }}</p>
      </div>

      <div class="byok-actions">
        <n-button
          v-if="configureAction !== 'unavailable'"
          type="primary"
          :disabled="busy"
          @click="openConfigure"
        >
          {{ configureAction === "update" ? t("更新配置") : t("配置 {client}", { client: label }) }}
        </n-button>
        <n-button
          v-if="recoverAvailable"
          secondary
          :disabled="busy"
          @click="recoverShown = true"
        >
          {{ t("恢复中断的写入") }}
        </n-button>
        <n-button
          v-if="removeAvailable"
          secondary
          :disabled="busy"
          @click="removeShown = true"
        >
          {{ view.adopted ? t("撤销接管") : t("移除 OCG 配置") }}
        </n-button>
      </div>
    </div>

    <ByokConfigureModal
      :show="configureShown"
      :client="client"
      :view="view"
      :target-path="appliedTarget"
      @update:show="configureShown = $event"
      @saved="onConfigured"
      @conflict="onConflict"
    />
    <ByokMutationModal
      mode="remove"
      :show="removeShown"
      :client="client"
      :view="view"
      :target-path="appliedTarget"
      @update:show="removeShown = $event"
      @saved="onRemoved"
      @conflict="onConflict"
    />
    <ByokMutationModal
      mode="recover"
      :show="recoverShown"
      :client="client"
      :view="view"
      :target-path="appliedTarget"
      @update:show="recoverShown = $event"
      @saved="onRecovered"
      @conflict="onConflict"
    />
  </section>
</template>

<script setup lang="ts">
import { computed, onActivated, onMounted, ref } from "vue";
import { NAlert, NButton, NInput, NSpin, NTag, useMessage } from "naive-ui";
import type { ByokApplicationView } from "../../api/byok-applications.ts";
import { t } from "../../i18n/index.ts";
import { useByokApplicationsStore } from "../../stores/byokApplications.ts";
import {
  BYOK_CLIENT_LABELS,
  HARNESS_DEFAULT_KEY_NAMES,
  byokConfigureAction,
  byokDisplayTarget,
  byokRecoverAvailable,
  byokRemoveAvailable,
  byokStatusPresentation,
  type ByokClientId,
} from "../../domain/byok-applications.ts";
import ByokConfigureModal from "./ByokConfigureModal.vue";
import ByokMutationModal from "./ByokMutationModal.vue";

const props = defineProps<{ client: ByokClientId }>();

const message = useMessage();
const store = useByokApplicationsStore();

const targetDraft = ref("");
const appliedTarget = ref<string | undefined>(undefined);
const actionError = ref("");
const configureShown = ref(false);
const removeShown = ref(false);
const recoverShown = ref(false);

const label = computed(() => BYOK_CLIENT_LABELS[props.client]);
const keyName = computed(() => HARNESS_DEFAULT_KEY_NAMES[props.client]);
const entry = computed(() => store.peek(props.client, appliedTarget.value));
const view = computed(() => entry.value?.view ?? null);
const busy = computed(() => Boolean(entry.value?.loading || entry.value?.mutating));
const initialLoading = computed(() => !entry.value || (entry.value.loading && !entry.value.view));
const presentation = computed(() => byokStatusPresentation(view.value?.status ?? "not_detected"));
const displayTarget = computed(() => byokDisplayTarget(view.value));
const configureAction = computed(() => byokConfigureAction(view.value));
const removeAvailable = computed(() => byokRemoveAvailable(view.value));
const recoverAvailable = computed(() => byokRecoverAvailable(view.value));

async function load(options: { retain?: boolean } = {}): Promise<void> {
  const target = appliedTarget.value;
  await store.inspect(props.client, target, options);
  const result = store.peek(props.client, target)?.view;
  // Adopt the backend-resolved exact path, but never stomp an in-progress edit.
  if (result?.configPath && targetDraft.value.trim() === (appliedTarget.value ?? "")) {
    targetDraft.value = result.configPath;
  }
}

function ensureLoaded(): void {
  const current = store.peek(props.client, appliedTarget.value);
  void load(current?.view ? { retain: true } : {});
}

function applyTarget(): void {
  if (busy.value) return;
  const next = targetDraft.value.trim() || undefined;
  actionError.value = "";
  configureShown.value = false; removeShown.value = false; recoverShown.value = false;
  if (next === appliedTarget.value) {
    void load({ retain: true });
    return;
  }
  appliedTarget.value = next;
  void load();
}

function openConfigure(): void {
  if (busy.value || !view.value || configureAction.value === "unavailable") return;
  actionError.value = "";
  configureShown.value = true;
}

function onConfigured(result: ByokApplicationView): void {
  configureShown.value = false;
  message.success(result.activationRequired
    ? t("配置已保存；重启 {client} 或开始新会话后生效。", { client: label.value })
    : t("配置已保存。"));
}

function onRemoved(_result: ByokApplicationView, adopted: boolean): void {
  removeShown.value = false;
  message.success(t(adopted ? "已撤销配置接管。" : "已移除 OCG 配置。"));
}

function onRecovered(): void {
  recoverShown.value = false;
  message.success(t("已恢复中断的写入。"));
}

function onConflict(): void {
  configureShown.value = false;
  removeShown.value = false;
  recoverShown.value = false;
  actionError.value = t("{client} 状态已变化，已刷新当前状态。", { client: label.value });
  void load({ retain: true }).catch(() => {});
}

onMounted(ensureLoaded);
onActivated(ensureLoaded);
</script>

<style scoped>
.byok-section {
  min-width: 0;
  display: grid;
  gap: var(--ocg-space-md);
}
.byok-state {
  min-height: 160px;
  display: grid;
  place-items: center;
}
.byok-body {
  min-width: 0;
  display: grid;
  gap: var(--ocg-space-md);
  padding: var(--ocg-space-lg);
  border: 1px solid var(--ocg-border);
  border-radius: var(--ocg-radius-lg);
  background: var(--ocg-surface);
  box-shadow: var(--ocg-shadow-sm);
}
.byok-toolbar {
  display: flex;
  justify-content: flex-end;
}
.byok-status-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--ocg-space-sm);
}
.byok-activation {
  color: var(--ocg-muted);
  font-size: var(--ocg-font-sm);
}
.byok-hint {
  margin: 0;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-sm);
  line-height: 1.6;
}
.byok-detail {
  margin: 0;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-sm);
  line-height: 1.6;
  overflow-wrap: anywhere;
}
.byok-target {
  display: grid;
  gap: var(--ocg-space-sm);
}
.byok-target-row {
  display: flex;
  gap: var(--ocg-space-sm);
  align-items: center;
}
.byok-selected-path {
  overflow-wrap: anywhere;
}
.byok-configured {
  display: grid;
  gap: var(--ocg-space-sm);
}
.byok-manual {
  display: grid;
  gap: var(--ocg-space-sm);
}
.byok-paths-title {
  margin: 0;
  color: var(--ocg-ink);
  font-size: var(--ocg-font-sm);
  font-weight: 600;
}
.byok-paths {
  margin: 0;
  padding-left: 20px;
  display: grid;
  gap: var(--ocg-space-xs);
  font-size: var(--ocg-font-sm);
}
.byok-paths code {
  overflow-wrap: anywhere;
}
.byok-actions {
  display: flex;
  flex-wrap: wrap;
  gap: var(--ocg-space-sm);
  margin-top: var(--ocg-space-xs);
}
</style>
