<template>
  <div class="applications-page">
    <h1 class="sr-only">{{ t("应用") }}</h1>
    <div
      v-if="initialLoading"
      class="applications-state"
      role="status"
      aria-live="polite"
      :aria-label="t('加载中…')"
    >
      <n-spin size="small" />
    </div>

    <n-alert
      v-else-if="dshStore.error && !dsh"
      type="error"
      :title="t('加载应用状态失败：{error}', { error: dshStore.error })"
    >
      <n-button size="small" secondary :loading="dshStore.loading" @click="load()">
        {{ t("重试") }}
      </n-button>
    </n-alert>

    <n-tabs
      v-else
      v-model:value="activeTab"
      type="line"
      class="applications-tabs"
      display-directive="if"
    >
      <template #suffix>
        <n-button secondary size="small" :loading="dshStore.loading" @click="load({ retain: true })">
          {{ t("刷新") }}
        </n-button>
      </template>
      <n-tab-pane name="dsh" tab="DSH">
        <section v-if="dsh" class="dsh-section" aria-labelledby="dsh-title">
          <h2 id="dsh-title" class="sr-only">DSH</h2>
          <n-alert
            v-if="dshStore.error"
            type="warning"
            :title="t('加载应用状态失败：{error}', { error: dshStore.error })"
          >
            <n-button size="small" secondary :loading="dshStore.loading" @click="load({ retain: true })">
              {{ t("重试") }}
            </n-button>
          </n-alert>
          <n-alert
            v-if="actionError && !installConfirmShown && !uninstallConfirmShown"
            type="error"
            :title="actionError"
            closable
            @close="actionError = ''"
          />

          <div class="dsh-status-row">
            <n-tag :type="presentation.tone" size="small">{{ t(presentation.labelKey) }}</n-tag>
            <span class="dsh-version">
              {{ t("当前版本") }}: <code>{{ dsh.version ?? t("未知") }}</code>
            </span>
          </div>
          <p class="dsh-hint">{{ t(presentation.hintKey) }}</p>
          <p v-if="hostDetail" class="dsh-detail">{{ hostDetail }}</p>
          <p v-if="outcomeHint" class="dsh-hint">{{ t(outcomeHint) }}</p>
          <p v-if="dsh.activationRequired" class="dsh-hint">
            {{ t("启动或重启 DSH，以导入所选 Key 并加载 OCG 插件。") }}
          </p>
          <p class="dsh-hint">
            {{ t("DSH 会动态同步完整的已鉴权 OCG /v1/models 目录，范围大于控制台中列出的应用模型。") }}
          </p>
          <p v-if="dsh.status === 'installed'" class="dsh-hint">
            {{ t("安装完成仅表示包注册成功，不代表模型连接已验证。") }}
          </p>

          <div class="dsh-discovery">
            <label for="dsh-profile-select" class="dsh-paths-title">{{ t("安装目标 Profile") }}</label>
            <n-select
              v-if="profileOptions.length > 0"
              id="dsh-profile-select"
              class="dsh-profile-select"
              :value="selectedProfilePath"
              :options="profileOptions"
              :disabled="dshStore.loading || dshStore.mutating"
              :aria-label="t('安装目标 Profile')"
              filterable
              @update:value="selectProfile"
            />
            <code v-if="selectedProfilePath" class="dsh-selected-path">{{ selectedProfilePath }}</code>
            <p class="dsh-hint">{{ t("所选 Profile 提供本机 DSH Home 与会话上下文；实际变更目标是显示的运行地址。") }}</p>
            <p v-if="dsh.discoveredProfiles.length === 0" class="dsh-hint">{{ t("在指定范围内未发现有效的 DSH Profile。") }}</p>
            <label for="dsh-runtime-url" class="dsh-paths-title">{{ t("DSH 运行地址") }}</label>
            <n-input
              id="dsh-runtime-url"
              class="dsh-runtime-input"
              :value="runtimeUrlDraft"
              :disabled="dshStore.loading || dshStore.mutating"
              :placeholder="suggestedUrl ?? ''"
              :aria-label="t('DSH 运行地址')"
              @update:value="runtimeUrlDraft = $event"
              @blur="load({ retain: true })"
            />
          </div>

          <div class="dsh-actions">
            <n-button
              type="primary"
              :loading="keysLoading"
              :disabled="installAction === 'unavailable'"
              @click="openInstall"
            >
              {{ installAction === "reinstall" ? t("重新安装到 {profile}", { profile: selectedProfileName }) : t("安装到 {profile}", { profile: selectedProfileName }) }}
            </n-button>
            <n-button
              v-if="uninstallAction === 'uninstall'"
              secondary
              :disabled="dshStore.mutating"
              @click="openUninstall"
            >
              {{ t("从 {profile} 卸载", { profile: selectedProfileName }) }}
            </n-button>
          </div>
        </section>
      </n-tab-pane>
    </n-tabs>

    <n-modal
      :show="installConfirmShown"
      preset="card"
      :title="installAction === 'reinstall' ? t('重新安装到 {profile}', { profile: selectedProfileName }) : t('安装到 {profile}', { profile: selectedProfileName })"
      class="dsh-install-modal"
      style="width: 520px; max-width: calc(100vw - 32px)"
      :mask-closable="false"
      :close-on-esc="!dshStore.mutating"
      @update:show="setInstallConfirmVisible"
    >
      <div v-if="dsh" class="dsh-confirm">
        <n-alert
          v-if="actionError"
          type="error"
          :title="actionError"
          closable
          @close="actionError = ''"
        />
        <div class="dsh-status-row">
          <n-tag :type="presentation.tone" size="small">{{ t(presentation.labelKey) }}</n-tag>
          <span class="dsh-version">
            {{ t("当前版本") }}: <code>{{ dsh.version ?? t("未知") }}</code>
          </span>
        </div>
        <p v-if="selectedProfileName === 'dsh-editor'" class="dsh-hint">
          {{ t("安装到 DSH Editor 前请先退出 Editor；安装后重新启动。") }}
        </p>
        <template v-if="dsh.runtimeUrl">
          <p class="dsh-paths-title">{{ t("DSH 运行地址") }}</p>
          <code class="dsh-selected-path">{{ displayedRuntimeUrl }}</code>
          <p class="dsh-hint">{{ t("将使用本机 DSH 会话操作所显示的运行地址。") }}</p>
          <p v-if="dsh.installed" class="dsh-hint">{{ t("将替换此地址上现有的 {package}，包括其他来源安装的同名包。", { package: "@open-console-gateway/dsh-plugin" }) }}</p>
        </template>
        <p class="dsh-paths-title">{{ t("安装将写入以下路径：") }}</p>
        <ul v-if="dsh.targetPaths.length > 0" class="dsh-paths">
          <li v-for="path in dsh.targetPaths" :key="path"><code>{{ path }}</code></li>
        </ul>
        <p v-else class="dsh-hint">{{ t("未报告目标路径") }}</p>
        <p id="dsh-key-label" class="dsh-paths-title">{{ t("用于 DSH 鉴权的 Key") }}</p>
        <n-radio-group
          v-model:value="selectedKeyId"
          class="dsh-key-group"
          aria-labelledby="dsh-key-label"
          :disabled="dshStore.mutating"
        >
          <n-radio v-for="option in keyOptions" :key="option.id" :value="option.id">
            {{ option.kind === "primary" ? t("主 Key") : option.name }}
          </n-radio>
        </n-radio-group>
        <p class="dsh-hint">
          {{ t("仅发送所选 Key 的标识；Key 明文不会显示，也不会随请求发送。") }}
        </p>
      </div>
      <template #footer>
        <div class="dsh-confirm-footer">
          <n-button quaternary :disabled="dshStore.mutating" @click="setInstallConfirmVisible(false)">
            {{ t("取消") }}
          </n-button>
          <n-button
            type="primary"
            :loading="dshStore.mutating"
            :disabled="!selectedKeyUsable"
            @click="confirmInstall"
          >
            {{ dshStore.mutating ? t("安装中…") : t("确认安装") }}
          </n-button>
        </div>
      </template>
    </n-modal>

    <n-modal
      :show="uninstallConfirmShown"
      preset="card"
      :title="t('从 {profile} 卸载', { profile: selectedProfileName })"
      class="dsh-uninstall-modal"
      style="width: 480px; max-width: calc(100vw - 32px)"
      :mask-closable="false"
      :close-on-esc="!dshStore.mutating"
      @update:show="setUninstallConfirmVisible"
    >
      <div v-if="dsh" class="dsh-confirm">
        <n-alert
          v-if="actionError"
          type="error"
          :title="actionError"
          closable
          @close="actionError = ''"
        />
        <p class="dsh-paths-title">{{ t("DSH 运行地址") }}</p>
        <code class="dsh-selected-path">{{ displayedRuntimeUrl }}</code>
        <p class="dsh-hint">{{ t("将使用本机 DSH 会话操作所显示的运行地址。") }}</p>
        <p class="dsh-hint">{{ t("卸载只移除 OCG 插件包，不会删除 DSH 凭据或 OCG Key。") }}</p>
        <p class="dsh-hint">{{ t("将移除此地址上的 {package}，包括其他来源安装的同名包。", { package: "@open-console-gateway/dsh-plugin" }) }}</p>
      </div>
      <template #footer>
        <div class="dsh-confirm-footer">
          <n-button quaternary :disabled="dshStore.mutating" @click="setUninstallConfirmVisible(false)">
            {{ t("取消") }}
          </n-button>
          <n-button
            type="primary"
            :loading="dshStore.mutating"
            @click="confirmUninstall"
          >
            {{ dshStore.mutating ? t("卸载中…") : t("确认卸载") }}
          </n-button>
        </div>
      </template>
    </n-modal>
  </div>
</template>

<script setup lang="ts">
import { computed, onActivated, onMounted, ref, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import {
  NAlert,
  NButton,
  NInput,
  NModal,
  NRadio,
  NRadioGroup,
  NSelect,
  NSpin,
  NTabPane,
  NTabs,
  NTag,
  useMessage,
} from "naive-ui";
import { DashboardRequestError, isRevisionConflict } from "../api/dashboard.ts";
import type { DshApplicationView } from "../api/dashboard-v4.ts";
import { t } from "../i18n/index.ts";
import { useConnectionStore } from "../stores/connection.ts";
import { useControlPlaneStore } from "../stores/controlPlane.ts";
import { useDshStore } from "../stores/dsh.ts";
import { useSessionStore } from "../stores/session.ts";
import { createRevalidateGate } from "../domain/revalidate.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";
import { useLocalizedModalCloseLabel } from "../utils/modal-close-label.ts";
import {
  DSH_APPLICATION_OUTCOME_KEYS,
  DSH_MUTATION_FEEDBACK_KEYS,
  buildDshInstallKeyOptions,
  dshHostDetail,
  dshInstallAction,
  dshMutationExpectation,
  dshMutationFeedback,
  dshStatusPresentation,
  dshUninstallAction,
  readApplicationTab,
  suggestedRuntimeUrl,
  type ApplicationTab,
} from "./dsh-application.ts";
import { routeQuerySearch } from "./app-navigation.ts";

const message = useMessage();
const route = useRoute();
const router = useRouter();
const connectionStore = useConnectionStore();
const controlPlane = useControlPlaneStore();
const sessionStore = useSessionStore();
const dshStore = useDshStore();
const revalidateGate = createRevalidateGate(60_000);
watch(() => sessionStore.authenticated, (ok) => { if (!ok) revalidateGate.reset(); });

const actionError = ref("");
const installConfirmShown = ref(false);
const uninstallConfirmShown = ref(false);
const keysLoading = ref(false);
const selectedKeyId = ref("");
const selectedProfilePath = ref("");
const runtimeUrlDraft = ref("");
const activeTab = ref<ApplicationTab>(readApplicationTab(routeQuerySearch("applications", route.query)));
let activatedOnce = false;

const dsh = computed(() => dshStore.application);
const initialLoading = computed(() => dshStore.loading && !dsh.value);
const presentation = computed(() => dshStatusPresentation(dsh.value?.status ?? "not_detected"));
const installAction = computed(() => (dsh.value && !dshStore.loading && selectedProfilePath.value === dsh.value.selectedProfilePath
  ? dshInstallAction(dsh.value)
  : "unavailable"));
const uninstallAction = computed(() => (dsh.value && !dshStore.loading && selectedProfilePath.value === dsh.value.selectedProfilePath
  ? dshUninstallAction(dsh.value)
  : "unavailable"));
const selectedProfileName = computed(() => dsh.value?.discoveredProfiles.find((profile) => profile.path === selectedProfilePath.value)?.name
  ?? selectedProfilePath.value.split(/[\\/]/).pop() ?? "web");
const suggestedUrl = computed(() => suggestedRuntimeUrl(selectedProfileName.value));
const displayedRuntimeUrl = computed(() => runtimeUrlDraft.value || dsh.value?.runtimeUrl || suggestedUrl.value || "");
const profileOptions = computed(() => {
  if (!dsh.value) return [];
  const options = dsh.value.discoveredProfiles.map((profile) => ({ label: `${profile.name} · ${profile.home}`, value: profile.path }));
  if (dsh.value.selectedProfilePath && !options.some((option) => option.value === dsh.value!.selectedProfilePath)) {
    options.unshift({ label: dsh.value.selectedProfilePath, value: dsh.value.selectedProfilePath });
  }
  return options;
});
const hostDetail = computed(() => dshHostDetail(dsh.value));
const outcomeHint = computed(() => {
  const outcome = dsh.value?.application;
  return outcome && outcome !== "applied" ? DSH_APPLICATION_OUTCOME_KEYS[outcome] : null;
});
const keyOptions = computed(() => buildDshInstallKeyOptions(connectionStore.info));
const selectedKeyUsable = computed(() =>
  keyOptions.value.some((option) => option.id === selectedKeyId.value),
);

async function load(options: { retain?: boolean } = {}): Promise<void> {
  await dshStore.load({
    profilePath: selectedProfilePath.value || undefined,
    runtimeUrl: runtimeUrlDraft.value || undefined,
    retain: options.retain,
  });
  const result = dshStore.application;
  if (!result) {
    selectedProfilePath.value = "";
    return;
  }
  selectedProfilePath.value = result.selectedProfilePath;
  if (result.runtimeUrl) runtimeUrlDraft.value = result.runtimeUrl;
  else if (!runtimeUrlDraft.value) runtimeUrlDraft.value = suggestedRuntimeUrl(selectedProfileName.value) ?? "";
}

function selectProfile(value: string | number | null): void {
  if (typeof value !== "string") return;
  if (dshStore.loading || dshStore.mutating || value === selectedProfilePath.value) return;
  selectedProfilePath.value = value;
  actionError.value = "";
  const name = dsh.value?.discoveredProfiles.find((profile) => profile.path === value)?.name
    ?? value.split(/[\\/]/).pop();
  runtimeUrlDraft.value = suggestedRuntimeUrl(name) ?? "";
  void load({ retain: true });
}

async function openInstall(): Promise<void> {
  if (!dsh.value || installAction.value === "unavailable" || keysLoading.value) return;
  actionError.value = "";
  keysLoading.value = true;
  try {
    if (!connectionStore.info) await connectionStore.load();
    selectedKeyId.value = keyOptions.value[0]?.id ?? "";
    if (!selectedKeyId.value) {
      actionError.value = t("没有可用于 DSH 的已启用 Key。");
      return;
    }
    installConfirmShown.value = true;
  } catch (error) {
    actionError.value = dashboardErrorDetail(error);
  } finally {
    keysLoading.value = false;
  }
}

function openUninstall(): void {
  if (!dsh.value || uninstallAction.value === "unavailable") return;
  actionError.value = "";
  uninstallConfirmShown.value = true;
}

function setInstallConfirmVisible(show: boolean): void {
  if (dshStore.mutating) return;
  installConfirmShown.value = show;
}

function setUninstallConfirmVisible(show: boolean): void {
  if (dshStore.mutating) return;
  uninstallConfirmShown.value = show;
}

function reportMutation(result: DshApplicationView, operation: "install" | "uninstall"): void {
  const feedback = dshMutationFeedback(result, operation);
  if (feedback === "success") {
    message.success(t(operation === "install" ? "DSH 安装完成" : "DSH 卸载完成"));
    return;
  }
  const hint = t(DSH_MUTATION_FEEDBACK_KEYS[feedback]);
  if (feedback === "restart-required") message.warning(hint);
  else {
    actionError.value = result.detail ? `${hint} ${result.detail}` : hint;
    message.warning(hint);
  }
}

async function confirmInstall(): Promise<void> {
  const app = dsh.value;
  const fingerprint = app?.fingerprint;
  if (!app || !fingerprint || dshStore.mutating || !selectedKeyUsable.value) return;
  actionError.value = "";
  try {

    const result = await controlPlane.runMutation(
      (expectation) => dshStore.install(
        {
          keyId: selectedKeyId.value,
          profilePath: app.selectedProfilePath,
          runtimeUrl: displayedRuntimeUrl.value || undefined,
          expectedFingerprint: fingerprint,
        },
        expectation,
      ),
      dshMutationExpectation(app),
    );
    selectedProfilePath.value = dshStore.application?.selectedProfilePath ?? selectedProfilePath.value;
    if (dshStore.application?.runtimeUrl) runtimeUrlDraft.value = dshStore.application.runtimeUrl;
    installConfirmShown.value = false;
    reportMutation(result, "install");
  } catch (error) {
    if (isRevisionConflict(error) || (error instanceof DashboardRequestError && error.status === 409)) {
      installConfirmShown.value = false;
      actionError.value = t("DSH 状态已变化，已刷新当前状态。");
      await load({ retain: true }).catch(() => {});
    } else {
      actionError.value = t("安装失败：{error}", { error: dashboardErrorDetail(error) });
    }
  }
}

async function confirmUninstall(): Promise<void> {
  const app = dsh.value;
  const fingerprint = app?.fingerprint;
  if (!app || !fingerprint || dshStore.mutating) return;
  actionError.value = "";
  try {

    const result = await controlPlane.runMutation(
      (expectation) => dshStore.uninstall(
        {
          profilePath: app.selectedProfilePath,
          runtimeUrl: displayedRuntimeUrl.value || undefined,
          expectedFingerprint: fingerprint,
        },
        expectation,
      ),
      dshMutationExpectation(app),
    );
    selectedProfilePath.value = dshStore.application?.selectedProfilePath ?? selectedProfilePath.value;
    uninstallConfirmShown.value = false;
    reportMutation(result, "uninstall");
  } catch (error) {
    if (isRevisionConflict(error) || (error instanceof DashboardRequestError && error.status === 409)) {
      uninstallConfirmShown.value = false;
      actionError.value = t("DSH 状态已变化，已刷新当前状态。");
      await load({ retain: true }).catch(() => {});
    } else {
      actionError.value = t("卸载失败：{error}", { error: dashboardErrorDetail(error) });
    }
  }
}

useLocalizedModalCloseLabel(installConfirmShown, "dsh-install-modal");
useLocalizedModalCloseLabel(uninstallConfirmShown, "dsh-uninstall-modal");

watch(activeTab, (tab) => {
  void router.replace({ query: { ...route.query, app: tab } });
});

onMounted(() => void load());
onActivated(() => {
  if (!activatedOnce) { activatedOnce = true; return; }
  if (dsh.value && !revalidateGate.shouldRun()) return;
  revalidateGate.record();
  void load({ retain: true });
});
</script>

<style scoped>
.applications-page {
  min-width: 0;
  max-width: 1060px;
  margin: 0 auto;
  overflow-x: hidden;
}
.applications-state {
  min-height: 160px;
  display: grid;
  place-items: center;
}
.applications-tabs :deep(.n-tabs-nav) {
  margin-bottom: var(--ocg-space-md);
}
.dsh-section {
  min-width: 0;
  display: grid;
  gap: var(--ocg-space-md);
  padding: var(--ocg-space-lg);
  border: 1px solid var(--ocg-border);
  border-radius: var(--ocg-radius-lg);
  background: var(--ocg-surface);
  box-shadow: var(--ocg-shadow-sm);
}
.dsh-status-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--ocg-space-sm);
}
.dsh-version {
  color: var(--ocg-muted);
  font-size: var(--ocg-font-sm);
}
.dsh-hint {
  margin: 0;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-sm);
  line-height: 1.6;
}
.dsh-detail {
  margin: 0;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-sm);
  line-height: 1.6;
  overflow-wrap: anywhere;
}
.dsh-actions {
  display: flex;
  gap: var(--ocg-space-sm);
  margin-top: var(--ocg-space-xs);
}

.dsh-discovery {
  display: grid;
  gap: var(--ocg-space-sm);
}
.dsh-selected-path {
  overflow-wrap: anywhere;
}
.dsh-confirm {
  display: grid;
  gap: var(--ocg-space-md);
}
.dsh-paths-title {
  margin: 0;
  color: var(--ocg-ink);
  font-size: var(--ocg-font-sm);
  font-weight: 600;
}
.dsh-paths {
  margin: 0;
  padding-left: 20px;
  display: grid;
  gap: var(--ocg-space-xs);
  font-size: var(--ocg-font-sm);
}
.dsh-paths code {
  overflow-wrap: anywhere;
}
.dsh-key-group {
  display: grid;
  gap: var(--ocg-space-sm);
}
.dsh-confirm-footer {
  display: flex;
  justify-content: flex-end;
  gap: var(--ocg-space-sm);
}
</style>
