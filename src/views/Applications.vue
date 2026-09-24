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
      v-else-if="loadError && !dsh"
      type="error"
      :title="t('加载应用状态失败：{error}', { error: loadError })"
    >
      <n-button size="small" secondary :loading="loading" @click="load()">
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
        <n-button secondary size="small" :loading="loading" @click="load({ retain: true })">
          {{ t("刷新") }}
        </n-button>
      </template>
      <n-tab-pane name="dsh" tab="DSH">
        <section v-if="dsh" class="dsh-section" aria-labelledby="dsh-title">
          <h2 id="dsh-title" class="sr-only">DSH</h2>
          <n-alert
            v-if="loadError"
            type="warning"
            :title="t('加载应用状态失败：{error}', { error: loadError })"
          >
            <n-button size="small" secondary :loading="loading" @click="load({ retain: true })">
              {{ t("重试") }}
            </n-button>
          </n-alert>
          <n-alert
            v-if="installError && !installConfirmShown"
            type="error"
            :title="t('安装失败：{error}', { error: installError })"
            closable
            @close="installError = ''"
          />

          <div class="dsh-status-row">
            <n-tag :type="presentation.tone" size="small">{{ t(presentation.labelKey) }}</n-tag>
            <span class="dsh-version">
              {{ t("当前版本") }}: <code>{{ dsh.version ?? t("未知") }}</code>
            </span>
          </div>
          <p class="dsh-hint">{{ t(presentation.hintKey) }}</p>
          <p v-if="hostDetail" class="dsh-detail">{{ hostDetail }}</p>
          <p v-if="dsh.activationRequired" class="dsh-hint">
            {{ t("启动或重启 DSH，以导入所选 Key 并加载 OCG 插件。") }}
          </p>
          <p class="dsh-hint">
            {{ t("DSH 会动态同步完整的已鉴权 OCG /v1/models 目录，范围大于控制台中列出的应用模型。") }}
          </p>
          <p v-if="dsh.status === 'installed'" class="dsh-hint">
            {{ t("安装完成仅表示包注册成功，不代表模型连接已验证。") }}
          </p>

          <div class="dsh-actions">
            <n-button
              type="primary"
              :loading="keysLoading"
              :disabled="installAction === 'unavailable'"
              @click="openInstall"
            >
              {{ installAction === "reinstall" ? t("重新安装 DSH") : t("安装 DSH") }}
            </n-button>
          </div>
        </section>
      </n-tab-pane>
    </n-tabs>

    <n-modal
      :show="installConfirmShown"
      preset="card"
      :title="installAction === 'reinstall' ? t('重新安装 DSH') : t('安装 DSH')"
      class="dsh-install-modal"
      style="width: 520px; max-width: calc(100vw - 32px)"
      :mask-closable="false"
      :close-on-esc="!installing"
      @update:show="setInstallConfirmVisible"
    >
      <div v-if="dsh" class="dsh-confirm">
        <n-alert
          v-if="installError"
          type="error"
          :title="t('安装失败：{error}', { error: installError })"
          closable
          @close="installError = ''"
        />
        <div class="dsh-status-row">
          <n-tag :type="presentation.tone" size="small">{{ t(presentation.labelKey) }}</n-tag>
          <span class="dsh-version">
            {{ t("当前版本") }}: <code>{{ dsh.version ?? t("未知") }}</code>
          </span>
        </div>
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
          :disabled="installing"
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
          <n-button quaternary :disabled="installing" @click="setInstallConfirmVisible(false)">
            {{ t("取消") }}
          </n-button>
          <n-button
            type="primary"
            :loading="installing"
            :disabled="!selectedKeyUsable"
            @click="confirmInstall"
          >
            {{ installing ? t("安装中…") : t("确认安装") }}
          </n-button>
        </div>
      </template>
    </n-modal>
  </div>
</template>

<script setup lang="ts">
import { computed, onActivated, onMounted, ref, watch } from "vue";
import {
  NAlert,
  NButton,
  NModal,
  NRadio,
  NRadioGroup,
  NSpin,
  NTabPane,
  NTabs,
  NTag,
  useMessage,
} from "naive-ui";
import { DashboardRequestError, isRevisionConflict } from "../api/dashboard.ts";
import { dashboardV4 } from "../api/dashboard-v4.ts";
import type { DshApplication } from "../api/generated/dashboard-v4.ts";
import { t } from "../i18n/index.ts";
import { useConnectionStore } from "../stores/connection.ts";
import { useControlPlaneStore } from "../stores/controlPlane.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";
import { useLocalizedModalCloseLabel } from "../utils/modal-close-label.ts";
import {
  buildDshInstallKeyOptions,
  dshHostDetail,
  dshInstallAction,
  dshInstallExpectation,
  dshStatusPresentation,
  readApplicationTab,
  type ApplicationTab,
} from "./dsh-application.ts";

const message = useMessage();
const connectionStore = useConnectionStore();
const controlPlane = useControlPlaneStore();

const dsh = ref<DshApplication | null>(null);
const loading = ref(false);
const loadError = ref("");
const installError = ref("");
const installConfirmShown = ref(false);
const installing = ref(false);
const keysLoading = ref(false);
const selectedKeyId = ref("");
const activeTab = ref<ApplicationTab>(readApplicationTab(window.location.search));
let activatedOnce = false;

const initialLoading = computed(() => loading.value && !dsh.value);
const presentation = computed(() => dshStatusPresentation(dsh.value?.status ?? "not_detected"));
const installAction = computed(() => (dsh.value ? dshInstallAction(dsh.value) : "unavailable"));
const hostDetail = computed(() => dshHostDetail(dsh.value));
const keyOptions = computed(() => buildDshInstallKeyOptions(connectionStore.info));
const selectedKeyUsable = computed(() =>
  keyOptions.value.some((option) => option.id === selectedKeyId.value),
);

async function load(options: { retain?: boolean } = {}): Promise<void> {
  if (loading.value) return;
  loading.value = true;
  if (!options.retain) loadError.value = "";
  try {
    try {
      dsh.value = await dashboardV4.getDshApplication();
      loadError.value = "";
    } catch (error) {
      loadError.value = dashboardErrorDetail(error);
    }
  } finally {
    loading.value = false;
  }
}

async function openInstall(): Promise<void> {
  if (!dsh.value || installAction.value === "unavailable" || keysLoading.value) return;
  installError.value = "";
  keysLoading.value = true;
  try {
    if (!connectionStore.info) await connectionStore.load();
    selectedKeyId.value = keyOptions.value[0]?.id ?? "";
    if (!selectedKeyId.value) {
      installError.value = t("没有可用于 DSH 的已启用 Key。");
      return;
    }
    installConfirmShown.value = true;
  } catch (error) {
    installError.value = dashboardErrorDetail(error);
  } finally {
    keysLoading.value = false;
  }
}

function setInstallConfirmVisible(show: boolean): void {
  if (installing.value) return;
  installConfirmShown.value = show;
}

async function confirmInstall(): Promise<void> {
  const app = dsh.value;
  const fingerprint = app?.fingerprint;
  if (!app || !fingerprint || installing.value || !selectedKeyUsable.value) return;
  installing.value = true;
  installError.value = "";
  try {
    if (!controlPlane.hasTokens()) await controlPlane.refresh();
    const result = await controlPlane.runMutation(
      (expectation) => dashboardV4.installDshApplication(
        { keyId: selectedKeyId.value, expectedFingerprint: fingerprint },
        expectation,
      ),
      dshInstallExpectation(app),
    );
    dsh.value = result;
    installConfirmShown.value = false;
    message.success(t("DSH 安装完成"));
  } catch (error) {
    if (isRevisionConflict(error) || (error instanceof DashboardRequestError && error.status === 409)) {
      installConfirmShown.value = false;
      installError.value = t("DSH 状态已变化，已刷新当前状态。");
      await load({ retain: true }).catch(() => {});
    } else {
      installError.value = dashboardErrorDetail(error);
    }
  } finally {
    installing.value = false;
  }
}

useLocalizedModalCloseLabel(installConfirmShown, "dsh-install-modal");

watch(activeTab, (tab) => {
  const url = new URL(window.location.href);
  url.searchParams.set("app", tab);
  window.history.replaceState(null, "", url);
});

onMounted(() => void load());
onActivated(() => {
  if (activatedOnce) void load({ retain: true });
  else activatedOnce = true;
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
