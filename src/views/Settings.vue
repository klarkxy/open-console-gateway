<template>
  <div class="settings-grid">
    <section class="settings-card" aria-labelledby="forwarding-title">
      <div class="settings-head">
        <div>
          <h2 id="forwarding-title"><n-icon class="section-icon" :component="SwapOutlined" aria-hidden="true" /> {{ t("转发") }}</h2>
        </div>
      </div>
      <n-form :model="config" label-placement="top" :show-feedback="false">
        <section class="settings-subsection proxy-settings" aria-labelledby="proxy-title">
          <h3 id="proxy-title">{{ t("出站代理") }}</h3>
          <n-radio-group
            v-model:value="config.proxy_mode"
            name="proxy-mode"
            class="proxy-mode-group"
            :disabled="!loaded || saving || testingProxy"
          >
            <n-radio value="auto">{{ t("自动（系统 / 环境）") }}</n-radio>
            <n-radio value="manual">{{ t("手动 HTTP 代理") }}</n-radio>
            <n-radio value="direct">{{ t("强制直连") }}</n-radio>
            <n-radio value="list">{{ t("按模型名单") }}</n-radio>
          </n-radio-group>
          <p class="field-caption proxy-mode-help">{{ proxyModeHelp }}</p>
          <n-form-item
            v-if="config.proxy_mode === 'manual' || config.proxy_mode === 'list'"
            :label="t('代理地址')"
            :show-feedback="true"
            :validation-status="proxyUrlPreview.status"
            :feedback="proxyUrlPreview.feedback"
          >
            <n-input
              v-model:value="config.proxy_url"
              class="mono"
              clearable
              :disabled="!loaded || saving || testingProxy"
              placeholder="http://127.0.0.1:7890"
              :input-props="{ 'aria-label': t('代理地址') }"
              @blur="normalizeProxyInput"
            />
          </n-form-item>
          <template v-if="config.proxy_mode === 'list'">
            <n-form-item :label="t('名单方向')">
              <n-radio-group
                v-model:value="config.proxy_list_direction"
                name="proxy-list-direction"
                class="proxy-direction-group"
                :disabled="!loaded || saving || testingProxy"
              >
                <n-radio value="whitelist">{{ t("白名单（名单内走代理）") }}</n-radio>
                <n-radio value="blacklist">{{ t("黑名单（名单内直连）") }}</n-radio>
              </n-radio-group>
            </n-form-item>
            <p class="field-caption proxy-direction-help">{{ proxyDirectionHelp }}</p>
            <n-form-item :label="t('名单内模型')">
              <div class="proxy-model-grid" role="group" :aria-label="t('名单内模型')">
                <label
                  v-for="model in config.proxy_supported_models"
                  :key="model.id"
                  class="proxy-model-option"
                  :class="{ 'proxy-model-free': isZenFreeModel(model.id) }"
                >
                  <n-checkbox
                    :checked="config.proxy_list_models.some((id) => proxyModelKey(id) === proxyModelKey(model.id))"
                    :disabled="!loaded || saving || testingProxy"
                    @update:checked="(checked: boolean) => toggleProxyListModel(model.id, checked)"
                  >
                    {{ model.id }}
                  </n-checkbox>
                  <span class="proxy-model-hint">{{ protocolLabel(model.preferred_protocol) }}</span>
                  <span v-if="isZenFreeModel(model.id)" class="proxy-model-free-hint">
                    {{ t("Zen free 额度按出口 IP 共享，走代理会改变额度归属") }}
                  </span>
                </label>
              </div>
            </n-form-item>
            <n-alert
              v-if="proxyUnknownModels.length > 0"
              type="warning"
              class="proxy-stale-note"
              :title="t('存储名单包含未知模型')"
            >{{ t("保存时将被忽略：{ids}", { ids: proxyUnknownModels.join("、") }) }}</n-alert>
          </template>
          <div class="proxy-test-row">
            <n-button
              secondary
              :loading="testingProxy"
              :disabled="!loaded || saving || proxyUrlPreview.status === 'error'"
              @click="testProxyConnection"
            >{{ t("测试连接") }}</n-button>
            <span class="field-caption">{{ proxyTestHelp }}</span>
          </div>
          <n-alert
            v-if="proxyTestResult"
            class="proxy-test-result"
            :type="proxyTestResult.type"
            :title="proxyTestResult.title"
          >{{ proxyTestResult.message }}</n-alert>
        </section>
        <div class="downstream-grid">
          <n-form-item :label="t('Gateway 端口')">
            <div class="gateway-port-field">
              <n-input-number
                v-model:value="config.gateway_port"
                :min="1"
                :max="65535"
                :precision="0"
                :disabled="!loaded || saving || config.gateway_port_from_env"
                :input-props="{ 'aria-label': t('Gateway 端口') }"
              ><template #minus-icon><span aria-hidden="true">−</span><span class="sr-only">{{ t('减少{field}', { field: t('Gateway 端口') }) }}</span></template>
              <template #add-icon><span aria-hidden="true">+</span><span class="sr-only">{{ t('增加{field}', { field: t('Gateway 端口') }) }}</span></template></n-input-number>
              <p v-if="config.gateway_port_from_env">
                {{ t("由环境变量 OCG_GATEWAY_PORT 管理，修改后重启生效。") }}
              </p>
            </div>
          </n-form-item>
          <n-form-item
            :label="t('下游访问根地址（可选）')"
            :show-feedback="true"
            :validation-status="clientRootPreview.status"
            :feedback="clientRootPreview.feedback"
          >
            <div class="client-root-field">
              <n-input
                v-model:value="clientRootInputValue"
                :disabled="!loaded"
                :readonly="config.client_root_url_from_env"
                :clearable="!config.client_root_url_from_env && !!config.client_root_url"
                :placeholder="config.client_root_url_from_env ? '' : automaticClientRootUrls.rootUrl"
                class="mono"
                :input-props="{
                  'aria-label': t('下游访问根地址（可选）'),
                  'aria-describedby': 'client-root-help',
                }"
                @blur="normalizeClientRootInput"
              />
              <p id="client-root-help">
                <template v-if="config.client_root_url_from_env">
                  {{ t("由环境变量 OCG_CLIENT_ROOT_URL 管理，修改后重启生效。") }}<br />
                </template>
                <span v-else-if="!config.client_root_url.trim()" class="sr-only">
                  {{ automaticClientRootFeedback }}
                </span>
              </p>
            </div>
          </n-form-item>
        </div>
        <section
          v-if="config.auto_start_supported"
          class="settings-subsection"
          aria-labelledby="startup-title"
        >
          <h3 id="startup-title">{{ t("开机启动") }}</h3>
          <n-switch
            :value="config.auto_start"
            @update:value="handleAutoStartToggle"
            :aria-label="t('随系统登录自动启动 Open Console Gateway')"
            :disabled="!loaded || saving"
            :loading="saving"
          >
            <template #checked>{{ t("开启") }}</template>
            <template #unchecked>{{ t("关闭") }}</template>
          </n-switch>
        </section>
        <section
          v-if="config.dock_visibility_supported"
          class="settings-subsection"
          aria-labelledby="dock-icon-title"
        >
          <h3 id="dock-icon-title">{{ t("Dock 图标") }}</h3>
          <n-switch
            :value="config.show_dock_icon"
            @update:value="handleDockVisibilityToggle"
            :aria-label="t('在 Dock 中显示 Open Console Gateway')"
            :disabled="!loaded || saving"
            :loading="saving"
          >
            <template #checked>{{ t("开启") }}</template>
            <template #unchecked>{{ t("关闭") }}</template>
          </n-switch>
        </section>
        <section class="settings-subsection" aria-labelledby="request-timeout-title">
          <h3 id="request-timeout-title">{{ t("请求超时") }}</h3>
          <n-form-item :label="t('连接超时')">
            <div class="timeout-field">
              <n-input-number
                v-model:value="config.connect_timeout_secs"
                :disabled="!loaded"
                :min="1"
                :max="300"
                :precision="0"
                :input-props="{ 'aria-label': t('连接超时（秒）') }"
              >
                <template #suffix>{{ t("秒") }}</template>
              <template #minus-icon><span aria-hidden="true">−</span><span class="sr-only">{{ t('减少{field}', { field: t('连接超时（秒）') }) }}</span></template>
              <template #add-icon><span aria-hidden="true">+</span><span class="sr-only">{{ t('增加{field}', { field: t('连接超时（秒）') }) }}</span></template></n-input-number>
            </div>
          </n-form-item>
          <n-form-item :label="t('非流式总超时')">
            <div class="timeout-field">
              <n-input-number
                v-model:value="config.non_stream_timeout_secs"
                :disabled="!loaded"
                :min="1"
                :max="3600"
                :precision="0"
                :input-props="{ 'aria-label': t('非流式总超时（秒）') }"
              >
                <template #suffix>{{ t("秒") }}</template>
              <template #minus-icon><span aria-hidden="true">−</span><span class="sr-only">{{ t('减少{field}', { field: t('非流式总超时（秒）') }) }}</span></template>
              <template #add-icon><span aria-hidden="true">+</span><span class="sr-only">{{ t('增加{field}', { field: t('非流式总超时（秒）') }) }}</span></template></n-input-number>
            </div>
          </n-form-item>
          <n-form-item :label="t('流式空闲超时')">
            <div class="timeout-field">
              <n-input-number
                v-model:value="config.stream_idle_timeout_secs"
                :disabled="!loaded"
                :min="1"
                :max="3600"
                :precision="0"
                :input-props="{ 'aria-label': t('流式空闲超时（秒）') }"
              >
                <template #suffix>{{ t("秒") }}</template>
              <template #minus-icon><span aria-hidden="true">−</span><span class="sr-only">{{ t('减少{field}', { field: t('流式空闲超时（秒）') }) }}</span></template>
              <template #add-icon><span aria-hidden="true">+</span><span class="sr-only">{{ t('增加{field}', { field: t('流式空闲超时（秒）') }) }}</span></template></n-input-number>
            </div>
          </n-form-item>
        </section>
      </n-form>
      <n-alert v-if="settingsLoadError" type="error" :title="t('设置加载失败，请重试')">
        <div class="settings-load-error">
          <span>{{ settingsLoadError }}</span>
          <n-button size="small" secondary @click="loadSettings">{{ t("重试") }}</n-button>
        </div>
      </n-alert>
      <n-button
        type="primary"
        :loading="saving"
        :disabled="!loaded || testingProxy || proxyUrlPreview.status === 'error' || clientRootPreview.status === 'error'"
        @click="saveSettings"
      >{{ t("保存设置") }}</n-button>
    </section>

    <TemporaryUnavailabilitySection />

    <div class="settings-side">
      <section class="settings-card" aria-labelledby="appearance-title">
        <div class="settings-head">
          <div>
            <h2 id="appearance-title"><n-icon class="section-icon" :component="BgColorsOutlined" aria-hidden="true" /> {{ t("外观") }}</h2>
          </div>
        </div>
        <div class="theme-grid" role="group" :aria-label="t('选择主题')">
          <button
            v-for="option in THEME_OPTIONS"
            :key="option.value"
            type="button"
            class="theme-option"
            :class="{ 'theme-option--selected': themeName === option.value }"
            :aria-pressed="themeName === option.value"
            @click="emit('update:themeName', option.value)"
          >
            <span
              class="theme-swatch"
              :class="{
                'theme-swatch--default': option.value === 'default',
                'theme-swatch--white': option.value === 'white',
              }"
              :style="{ background: option.swatch }"
              aria-hidden="true"
            />
            <span>{{ t(option.label as MessageKey) }}</span>
            <n-icon
              v-if="themeName === option.value"
              class="theme-check"
              :component="CheckOutlined"
              aria-hidden="true"
            />
          </button>
        </div>
      </section>

      <section class="settings-card" aria-labelledby="update-title">
        <div class="settings-head">
          <div>
            <h2 id="update-title"><n-icon class="section-icon" :component="CloudSyncOutlined" aria-hidden="true" /> {{ t("检查更新") }}</h2>
          </div>
        </div>
        <n-button
          type="primary"
          :loading="checkingUpdate"
          :disabled="checkingUpdate || updateBusy"
          @click="checkForUpdate"
        >{{ checkingUpdate ? t("正在检查更新…") : t("检查更新") }}</n-button>
        <div class="update-result">
          <span v-if="updateAnnouncement" class="sr-only" aria-live="polite" aria-atomic="true">{{ updateAnnouncement }}</span>
          <n-alert
            v-if="updateResult"
            :type="updateResult.update_available ? 'warning' : 'success'"
            :title="t(updateResult.update_available ? '发现新版本' : '已是最新版本')"
          >
            <div class="update-result-content">
              <dl class="update-versions">
                <div>
                  <dt>{{ t("当前版本") }}</dt>
                  <dd><code>v{{ updateResult.current_version }}</code></dd>
                </div>
                <div>
                  <dt>{{ t("最新版本") }}</dt>
                  <dd><code>v{{ updateResult.latest_version }}</code></dd>
                </div>
              </dl>
              <div class="update-actions">
                <n-popconfirm
                  v-if="supportsInstallUpdate"
                  :positive-text="t('开始升级')"
                  :negative-text="t('取消')"
                  @positive-click="installAvailableUpdate"
                >
                  <template #trigger>
                    <n-button
                      type="primary"
                      size="small"
                      :loading="updateBusy"
                      :disabled="updateBusy"
                    >{{ t("下载并安装") }}</n-button>
                  </template>
                  {{ t("将下载并安装 v{version}，安装时 Open Console Gateway 会短暂退出并自动重启，继续吗？", {
                    version: updateResult.latest_version,
                  }) }}
                </n-popconfirm>
                <n-button
                  tag="a"
                  :type="supportsInstallUpdate ? 'default' : 'primary'"
                  :secondary="supportsInstallUpdate"
                  size="small"
                  :href="updateResult.release_url"
                  target="_blank"
                  rel="noopener noreferrer"
                >{{ t("查看发布页") }}</n-button>
              </div>
            </div>
          </n-alert>
          <n-alert
            v-if="activeUpdateStatus"
            :type="updateStatusAlertType"
            :title="updateStatusTitle"
          >
            <div class="update-status-body">
              <n-progress
                v-if="activeUpdateStatus.phase === 'downloading'"
                type="line"
                :height="8"
                :percentage="updateDownloadPercentage ?? 0"
                :processing="updateDownloadPercentage === null"
                :show-indicator="updateDownloadPercentage !== null"
              />
              <p v-if="activeUpdateStatus.phase === 'installing' || waitingForRestart">
                {{ t("Open Console Gateway 会短暂离线并自动重启。") }}
              </p>
              <p v-if="activeUpdateStatus.phase === 'failed'">
                {{ activeUpdateStatus.error || t("升级未完成，请重试。") }}
              </p>
              <n-popconfirm
                v-if="activeUpdateStatus.phase === 'failed' && supportsInstallUpdate"
                :positive-text="t('开始升级')"
                :negative-text="t('取消')"
                @positive-click="installAvailableUpdate"
              >
                <template #trigger>
                  <n-button size="small" type="primary">{{ t("重试升级") }}</n-button>
                </template>
                {{ t("将下载并安装 v{version}，安装时 Open Console Gateway 会短暂退出并自动重启，继续吗？", {
                  version: updateResult?.latest_version || updateTargetVersion,
                }) }}
              </n-popconfirm>
            </div>
          </n-alert>
          <n-alert v-if="updateError && !updateResult" type="error" :title="t('检查更新失败')">
            {{ updateError }}
          </n-alert>
        </div>
      </section>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, onActivated, onMounted, onUnmounted, ref, watch } from "vue";
import {
  NAlert,
  NButton,
  NCheckbox,
  NForm,
  NFormItem,
  NIcon,
  NInput,
  NInputNumber,
  NPopconfirm,
  NProgress,
  NRadio,
  NRadioGroup,
  NSwitch,
  useMessage,
} from "naive-ui";
import {
  CheckOutlined,
  SwapOutlined,
  BgColorsOutlined,
  CloudSyncOutlined,
} from "@vicons/antd";
import { DashboardRequestError, dashboardApi } from "../api/dashboard";
import { useSettingsStore } from "../stores/settings.ts";
import { useSessionStore } from "../stores/session.ts";
import { createRevalidateGate } from "../domain/revalidate.ts";
import type {
  AppConfig,
  ProxyMode,
  UpdateCheckResult,
  UpdateStatus,
} from "../api/dashboard";
import { THEME_OPTIONS } from "../theme";
import type { ResolvedTheme, ThemeName } from "../theme";
import { t } from "../i18n/index.ts";
import type { MessageKey } from "../i18n/index.ts";
import {
  normalizeClientRootUrl,
  resolveConnectionUrls,
} from "./dashboard-connection";
import { DEFAULT_OPENCODE_INVITE_URL } from "../domain/managed-account.ts";
import { mergeUnsavedSettings } from "./settings-merge";
import { normalizeProxyUrl, proxyModelKey, validateProxyList } from "./settings-proxy";
import {
  clearUpdateTarget,
  decideInstallRequestFailure,
  decideUpdateStatus,
  isUpdatePhaseBusy,
  readUpdateTarget,
  writeUpdateTarget,
} from "./settings-update-state";
import TemporaryUnavailabilitySection from "../components/TemporaryUnavailabilitySection.vue";

const { themeName } = defineProps<{
  themeName: ThemeName;
  resolvedTheme: ResolvedTheme;
}>();
const emit = defineEmits<{ "update:themeName": [value: ThemeName] }>();

const message = useMessage();
const settingsStore = useSettingsStore();
const sessionStore = useSessionStore();
const revalidateGate = createRevalidateGate(60_000);
watch(() => sessionStore.authenticated, (ok) => { if (!ok) revalidateGate.reset(); });
const saving = ref(false);
const testingProxy = ref(false);
const proxyTestResult = ref<{
  type: "success" | "error";
  title: string;
  message: string;
} | null>(null);
const loaded = ref(false);
const settingsLoadError = ref("");
const checkingUpdate = ref(false);
const updateResult = ref<UpdateCheckResult | null>(null);
const updateError = ref("");
const updateStatus = ref<UpdateStatus | null>(null);
const updateTargetVersion = ref("");
const waitingForRestart = ref(false);
const recoveringUpdate = ref(true);
const startingUpdate = ref(false);
const finishingUpdate = ref(false);
let updatePollTimer: number | undefined;
let updatePollDeadline = 0;
let updatePollGeneration = 0;
let updateDisposed = true;
let settingsLoadGeneration = 0;

const UPDATE_POLL_INTERVAL_MS = 1_000;
const UPDATE_INSTALL_TIMEOUT_MS = 15 * 60_000;
const savedConfig = ref<AppConfig | null>(null);
let pendingSettingsMerge: { current: AppConfig; saved: AppConfig } | null = null;

// ponytail: keep this pre-load fallback in sync with AppConfig::default().
const config = ref<AppConfig>({
  revision: 0,
  gateway_port: 9042,
  gateway_port_from_env: false,
  proxy_mode: "auto",
  proxy_url: "",
  proxy_list_direction: "whitelist",
  proxy_list_models: [],
  proxy_supported_models: [],
  opencode_invite_url: DEFAULT_OPENCODE_INVITE_URL,
  client_root_url: "",
  client_root_url_from_env: false,
  auto_start: false,
  auto_start_supported: false,
  show_dock_icon: true,
  dock_visibility_supported: false,
  connect_timeout_secs: 30,
  non_stream_timeout_secs: 900,
  stream_idle_timeout_secs: 300,
  routing_mode: "strict-priority",
  conversation_sticky: false,
});

const proxyModeHelp = computed(() => {
  const help: Record<ProxyMode, MessageKey> = {
    auto: "自动读取 HTTP_PROXY、HTTPS_PROXY、ALL_PROXY、NO_PROXY；Windows 也会读取系统代理，未配置时直连。",
    manual: "所有 HTTP 与 HTTPS 目标都走此代理；代理不可用时直接报错，不会静默回退直连。",
    direct: "忽略系统代理和代理环境变量，始终直接连接。",
    list: "按模型名单分流：仅名单内模型按方向走代理或直连；“测试连接”验证方向默认段。",
  };
  return t(help[config.value.proxy_mode]);
});

const proxyTestHelp = computed(() => (
  config.value.proxy_mode === "list"
    ? t("测试当前表单值，不会保存设置；仅验证方向默认段，不代表名单内模型的真实转发路径。")
    : t("测试当前表单值，不会保存设置；收到任意 HTTP 响应即表示链路可用。")
));

const proxyDirectionHelp = computed(() => (
  config.value.proxy_list_direction === "whitelist"
    ? t("名单内模型走代理，名单外模型直连；非聊天出站（价格 / 用量 / 升级检查）改为直连。")
    : t("名单内模型直连，名单外模型走代理；非聊天出站（价格 / 用量 / 升级检查）走代理。")
));

const proxySupportedIds = computed(() =>
  config.value.proxy_supported_models.map((model) => model.id),
);

/** Stored ids the current registry no longer knows; inert and dropped on save. */
const proxyUnknownModels = computed(() => (
  config.value.proxy_mode === "list"
    ? config.value.proxy_list_models.filter((id) => !proxySupportedIds.value.some((known) => proxyModelKey(known) === proxyModelKey(id)))
    : []
));

/** On the registered Zen free channel (egress-IP-shared quota). Go catalog
 * ids may end in `-free` without being on the free channel, so the hint must
 * follow the registry flag, not the suffix. */
function isZenFreeModel(id: string): boolean {
  return config.value.proxy_supported_models.some((model) => proxyModelKey(model.id) === proxyModelKey(id) && model.zen_free);
}

function protocolLabel(protocol: string): string {
  if (protocol === "chat_completions") return "Chat";
  if (protocol === "messages") return "Messages";
  if (protocol === "gemini") return "Gemini";
  return "Responses";
}

function toggleProxyListModel(id: string, checked: boolean) {
  const models = config.value.proxy_list_models.filter((model) => proxyModelKey(model) !== proxyModelKey(id));
  if (checked) models.push(id);
  config.value.proxy_list_models = models;
}

const proxyUrlPreview = computed<{ status?: "error"; feedback: string }>(() => {
  try {
    normalizeProxyUrl(config.value.proxy_mode, config.value.proxy_url);
    return {
      feedback: config.value.proxy_mode === "manual" || config.value.proxy_mode === "list"
        ? t("支持 http:// 或 https:// 代理地址，不支持在 URL 中保存用户名和密码。")
        : "",
    };
  } catch (error) {
    return {
      status: "error",
      feedback: error instanceof Error ? t(error.message as MessageKey) : t("代理地址格式无效"),
    };
  }
});

watch(
  () => [
    config.value.proxy_mode,
    config.value.proxy_url,
    config.value.proxy_list_direction,
    config.value.proxy_list_models,
  ],
  () => { proxyTestResult.value = null; },
);

const automaticClientRootUrls = computed(() => resolveConnectionUrls(
  "",
  window.location.origin,
  config.value.gateway_port,
  import.meta.env.DEV,
));
const automaticClientRootFeedback = computed(() => t(
  "未配置时自动使用：{root}（API Base URL：{api}）；自动值不会写入设置。",
  {
    root: automaticClientRootUrls.value.rootUrl,
    api: automaticClientRootUrls.value.apiBaseUrl,
  },
));

const clientRootInputValue = computed({
  get: () => config.value.client_root_url,
  set: (value: string) => {
    if (!config.value.client_root_url_from_env) config.value.client_root_url = value;
  },
});

const clientRootPreview = computed<{
  status?: "error" | "warning";
  feedback: string;
}>(() => {
  try {
    const urls = resolveConnectionUrls(
      config.value.client_root_url,
      window.location.origin,
      config.value.gateway_port,
      import.meta.env.DEV,
    );
    if (urls.insecureHttp) {
      return {
        status: "warning",
        feedback: t("API Base URL：{url}。警告：非本机 HTTP 会明文传输 Key 与请求内容。", { url: urls.apiBaseUrl }),
      };
    }
    if (!config.value.client_root_url.trim()) {
      return { feedback: automaticClientRootFeedback.value };
    }
    return { feedback: t("API Base URL：{url}", { url: urls.apiBaseUrl }) };
  } catch (error) {
    return {
      status: "error",
      feedback: error instanceof Error ? error.message : t("地址格式无效"),
    };
  }
});

const supportsInstallUpdate = computed(() => Boolean(
  updateResult.value?.update_available && updateResult.value.install_supported,
));
const activeUpdateStatus = computed(() => (
  updateStatus.value && updateStatus.value.phase !== "idle" ? updateStatus.value : null
));
const updateBusy = computed(() => {
  const phase = activeUpdateStatus.value?.phase;
  return recoveringUpdate.value
    || startingUpdate.value
    || finishingUpdate.value
    || waitingForRestart.value
    || (phase !== undefined && isUpdatePhaseBusy(phase));
});
const updateStatusAlertType = computed(() => {
  if (activeUpdateStatus.value?.phase === "failed") return "error";
  if (activeUpdateStatus.value?.phase === "installing" || waitingForRestart.value) return "warning";
  return "info";
});
const updateStatusTitle = computed(() => {
  if (waitingForRestart.value) return t("正在等待新版本启动…");
  switch (activeUpdateStatus.value?.phase) {
    case "checking":
      return t("正在准备升级…");
    case "downloading":
      return updateTargetVersion.value
        ? t("正在下载 v{version}…", { version: updateTargetVersion.value })
        : t("正在下载升级…");
    case "installing":
      return updateTargetVersion.value
        ? t("正在安装 v{version}…", { version: updateTargetVersion.value })
        : t("正在安装升级…");
    case "failed":
      return t("升级失败");
    default:
      return "";
  }
});
const updateAnnouncement = computed(() => {
  if (activeUpdateStatus.value) return updateStatusTitle.value;
  if (updateResult.value) {
    return t(updateResult.value.update_available ? "发现新版本" : "已是最新版本");
  }
  return updateError.value ? t("检查更新失败") : "";
});
const updateDownloadPercentage = computed(() => {
  const status = activeUpdateStatus.value;
  if (status?.phase !== "downloading" || status.total === null || status.total <= 0) return null;
  return Math.min(100, Math.max(0, Math.round((status.downloaded / status.total) * 100)));
});

async function loadSettings(): Promise<boolean> {
  const generation = ++settingsLoadGeneration;
  settingsLoadError.value = "";
  try {
    const nextConfig = await settingsStore.loadPresented();
    if (generation !== settingsLoadGeneration) return false;
    acceptSettingsSnapshot(nextConfig);
    return true;
  } catch (e) {
    if (generation !== settingsLoadGeneration) return false;
    settingsLoadError.value = e instanceof Error ? e.message : String(e);
    message.error(t("加载设置失败：{error}", { error: settingsLoadError.value }));
    return false;
  }
}

async function reloadSettingsAfterConflict(
  error: unknown,
  current = { ...config.value },
  saved = savedConfig.value ? { ...savedConfig.value } : null,
): Promise<boolean> {
  if (!(error instanceof DashboardRequestError) || error.status !== 409) return false;
  pendingSettingsMerge = saved ? { current, saved } : null;
  if (await loadSettings()) {
    message.warning(t("设置已被其他操作修改，已合并最新设置并保留本地修改，请再次保存"));
  } else {
    pendingSettingsMerge = null;
    message.error(t("保存失败：{error}", { error: String(error) }));
  }
  return true;
}

async function saveSettings() {
  if (!loaded.value) return;
  if (!validateGatewayPort()) return;
  if (!normalizeClientRootInput()) return;
  if (!normalizeProxyInput()) return;
  if (!normalizeProxyListInput()) return;
  if (!validateTimeouts()) return;
  saving.value = true;
  const payload = { ...config.value };
  const saved = savedConfig.value ? { ...savedConfig.value } : null;
  try {
    const result = await settingsStore.putPresented(payload);
    payload.revision = result.revision;
    config.value.revision = result.revision;
    savedConfig.value = { ...payload };
    message.success(t("设置已保存"));
  } catch (e) {
    if (!(await reloadSettingsAfterConflict(e, payload, saved))) {
      message.error(t("保存失败：{error}", { error: String(e) }));
    }
  } finally {
    saving.value = false;
  }
}

function validateGatewayPort(): boolean {
  if (config.value.gateway_port_from_env) return true;
  const port = config.value.gateway_port;
  if (Number.isInteger(port) && port >= 1 && port <= 65535) return true;
  message.error(t("Gateway 端口必须为 1–65535 的整数"));
  return false;
}

function normalizeProxyInput(): boolean {
  try {
    config.value.proxy_url = normalizeProxyUrl(config.value.proxy_mode, config.value.proxy_url);
    return true;
  } catch (error) {
    message.error(error instanceof Error ? t(error.message as MessageKey) : t("代理地址格式无效"));
    return false;
  }
}

/** Pre-save list validation: stale stored ids are dropped (never rendered by
 * the checkbox grid), then the non-empty rule is enforced like the API. */
function normalizeProxyListInput(): boolean {
  if (config.value.proxy_mode !== "list") return true;
  const supported = proxySupportedIds.value;
  const knownOnly = config.value.proxy_list_models
    .map((id) => id.trim())
    .filter((id) => supported.some((known) => proxyModelKey(known) === proxyModelKey(id)));
  try {
    config.value.proxy_list_models = validateProxyList(config.value.proxy_mode, knownOnly, supported);
    return true;
  } catch (error) {
    message.error(error instanceof Error ? t(error.message as MessageKey) : t("代理地址格式无效"));
    return false;
  }
}

async function testProxyConnection() {
  if (!loaded.value || testingProxy.value || !normalizeProxyInput()) return;
  const request = {
    proxy_mode: config.value.proxy_mode,
    proxy_url: config.value.proxy_url,
    proxy_list_direction: config.value.proxy_list_direction,
  };
  testingProxy.value = true;
  proxyTestResult.value = null;
  try {
    const result = await dashboardApi.testProxy(request);
    if (
      config.value.proxy_mode !== request.proxy_mode
      || config.value.proxy_url !== request.proxy_url
      || config.value.proxy_list_direction !== request.proxy_list_direction
    ) {
      return;
    }
    proxyTestResult.value = {
      type: "success",
      title: t("连接成功"),
      message: t("收到 HTTP {status} 响应，耗时 {latency} ms。", {
        status: result.status,
        latency: result.latency_ms,
      }),
    };
  } catch (error) {
    if (
      config.value.proxy_mode !== request.proxy_mode
      || config.value.proxy_url !== request.proxy_url
      || config.value.proxy_list_direction !== request.proxy_list_direction
    ) {
      return;
    }
    proxyTestResult.value = {
      type: "error",
      title: t("连接失败"),
      message: error instanceof Error ? error.message : String(error),
    };
  } finally {
    testingProxy.value = false;
  }
}

async function handleAutoStartToggle(newValue: boolean) {
  if (!loaded.value || !savedConfig.value) return;
  const saved = { ...savedConfig.value };
  const current = { ...config.value, auto_start: newValue };
  const next = { ...saved, auto_start: newValue };
  saving.value = true;
  try {
    const result = await settingsStore.putPresented(next);
    next.revision = result.revision;
    savedConfig.value = { ...next };
    config.value.auto_start = newValue;
    config.value.revision = result.revision;
    message.success(t("设置已保存"));
  } catch (e) {
    if (!(await reloadSettingsAfterConflict(e, current, saved))) {
      config.value.auto_start = savedConfig.value.auto_start;
      message.error(t("自动启动设置失败：{error}", { error: String(e) }));
    }
  } finally {
    saving.value = false;
  }
}

async function handleDockVisibilityToggle(newValue: boolean) {
  if (!loaded.value || !savedConfig.value) return;
  const saved = { ...savedConfig.value };
  const current = { ...config.value, show_dock_icon: newValue };
  const next = { ...saved, show_dock_icon: newValue };
  saving.value = true;
  try {
    const result = await settingsStore.putPresented(next);
    next.revision = result.revision;
    savedConfig.value = { ...next };
    config.value.show_dock_icon = newValue;
    config.value.revision = result.revision;
    message.success(t("设置已保存"));
  } catch (e) {
    if (!(await reloadSettingsAfterConflict(e, current, saved))) {
      config.value.show_dock_icon = savedConfig.value.show_dock_icon;
      message.error(t("Dock 图标设置失败：{error}", { error: String(e) }));
    }
  } finally {
    saving.value = false;
  }
}

function normalizeClientRootInput(): boolean {
  if (config.value.client_root_url_from_env) return true;
  try {
    config.value.client_root_url = normalizeClientRootUrl(config.value.client_root_url);
    return true;
  } catch (error) {
    message.error(error instanceof Error ? error.message : t("下游访问根地址无效"));
    return false;
  }
}

function validateTimeouts(): boolean {
  const fields = [
    { field: t("连接超时"), value: config.value.connect_timeout_secs, min: 1, max: 300 },
    { field: t("非流式总超时"), value: config.value.non_stream_timeout_secs, min: 1, max: 3600 },
    { field: t("流式空闲超时"), value: config.value.stream_idle_timeout_secs, min: 1, max: 3600 },
  ];
  const invalid = fields.find(({ value, min, max }) => (
    !Number.isInteger(value) || value < min || value > max
  ));
  if (!invalid) return true;
  message.error(t("{field}必须是 {min}–{max} 秒之间的整数", invalid));
  return false;
}

function acceptSettingsSnapshot(latest: AppConfig) {
  const pending = pendingSettingsMerge;
  savedConfig.value = { ...latest };
  config.value = pending
    ? mergeUnsavedSettings(latest, pending.current, pending.saved)
    : latest;
  pendingSettingsMerge = null;
  loaded.value = true;
  settingsLoadError.value = "";
}

async function checkForUpdate() {
  if (checkingUpdate.value) return;
  if (!updateBusy.value) {
    updateStatus.value = null;
    waitingForRestart.value = false;
  }
  checkingUpdate.value = true;
  updateResult.value = null;
  updateError.value = "";
  try {
    const result = await dashboardApi.checkForUpdate();
    if (!updateDisposed) updateResult.value = result;
  } catch (error) {
    if (!updateDisposed) {
      updateError.value = error instanceof Error ? error.message : String(error);
    }
  } finally {
    if (!updateDisposed) checkingUpdate.value = false;
  }
}

function updateStatusFallback(
  phase: UpdateStatus["phase"],
  error: string | null = null,
): UpdateStatus {
  return {
    phase,
    downloaded: updateStatus.value?.downloaded ?? 0,
    total: updateStatus.value?.total ?? null,
    error,
    current_version: updateStatus.value?.current_version
      ?? updateResult.value?.current_version
      ?? "",
    install_supported: updateStatus.value?.install_supported
      ?? updateResult.value?.install_supported
      ?? false,
  };
}

function sessionUpdateStorage(): Storage | null {
  try {
    return window.sessionStorage;
  } catch {
    return null;
  }
}

function clearPersistedUpdateTarget() {
  clearUpdateTarget(sessionUpdateStorage());
  updateTargetVersion.value = "";
}

function rememberUpdateTarget(version: string): string {
  const target = writeUpdateTarget(sessionUpdateStorage(), version);
  updateTargetVersion.value = target;
  return target;
}

function cancelUpdatePolling() {
  updatePollGeneration += 1;
  if (updatePollTimer !== undefined) {
    window.clearTimeout(updatePollTimer);
    updatePollTimer = undefined;
  }
}

function failUpdate(error: string) {
  cancelUpdatePolling();
  clearPersistedUpdateTarget();
  recoveringUpdate.value = false;
  startingUpdate.value = false;
  finishingUpdate.value = false;
  waitingForRestart.value = false;
  updateStatus.value = updateStatusFallback("failed", error);
}

function isActiveUpdateGeneration(generation: number): boolean {
  return !updateDisposed && generation === updatePollGeneration;
}

function scheduleUpdatePoll(generation: number, delay = UPDATE_POLL_INTERVAL_MS) {
  if (!isActiveUpdateGeneration(generation)) return;
  if (updatePollTimer !== undefined) window.clearTimeout(updatePollTimer);
  updatePollTimer = window.setTimeout(() => {
    updatePollTimer = undefined;
    void pollUpdateStatus(generation);
  }, delay);
}

function startUpdatePolling(delay = UPDATE_POLL_INTERVAL_MS): number {
  cancelUpdatePolling();
  updatePollDeadline = Date.now() + UPDATE_INSTALL_TIMEOUT_MS;
  const generation = updatePollGeneration;
  scheduleUpdatePoll(generation, delay);
  return generation;
}

function finishInstalledUpdate(status: UpdateStatus) {
  cancelUpdatePolling();
  clearPersistedUpdateTarget();
  const installedVersion = status.current_version;
  recoveringUpdate.value = false;
  startingUpdate.value = false;
  finishingUpdate.value = true;
  waitingForRestart.value = false;
  updateStatus.value = null;
  message.success(t("已升级到 v{version}", { version: installedVersion }));
  window.setTimeout(() => {
    window.location.reload();
  }, 800);
}

function observeUpdateStatusFailure() {
  if (updateStatus.value?.phase !== "installing" && !waitingForRestart.value) return;
  waitingForRestart.value = true;
  updateStatus.value = updateStatusFallback("installing");
}

function acceptObservedUpdateStatus(status: UpdateStatus): boolean {
  updateStatus.value = status;
  waitingForRestart.value = false;
  switch (decideUpdateStatus(status, updateTargetVersion.value)) {
    case "complete":
      finishInstalledUpdate(status);
      return true;
    case "failed":
      failUpdate(status.error || t("升级未完成，请重试。"));
      return true;
    case "busy":
      return false;
    case "idle":
      if (updateTargetVersion.value) {
        failUpdate(t("升级未完成，请重试。"));
      } else {
        cancelUpdatePolling();
        updateStatus.value = null;
      }
      return true;
  }
}

async function pollUpdateStatus(generation: number) {
  if (!isActiveUpdateGeneration(generation)) return;
  if (Date.now() >= updatePollDeadline) {
    failUpdate(t("等待新版本启动超时。请确认安装是否被安全软件拦截，然后重试。"));
    return;
  }
  try {
    const status = await dashboardApi.getUpdateStatus();
    if (!isActiveUpdateGeneration(generation)) return;
    if (acceptObservedUpdateStatus(status)) return;
  } catch {
    if (!isActiveUpdateGeneration(generation)) return;
    // A transient status request failure must not turn checking/downloading
    // into a false installation state. Only retain an already observed restart.
    observeUpdateStatusFailure();
  }
  scheduleUpdatePoll(generation);
}

async function restoreUpdateState() {
  recoveringUpdate.value = true;
  cancelUpdatePolling();
  const generation = updatePollGeneration;
  updateTargetVersion.value = readUpdateTarget(sessionUpdateStorage());
  try {
    const status = await dashboardApi.getUpdateStatus();
    if (!isActiveUpdateGeneration(generation)) return;
    recoveringUpdate.value = false;
    if (acceptObservedUpdateStatus(status)) return;
    startUpdatePolling();
  } catch {
    if (!isActiveUpdateGeneration(generation)) return;
    recoveringUpdate.value = false;
    if (!updateTargetVersion.value) return;
    waitingForRestart.value = true;
    updateStatus.value = updateStatusFallback("installing");
    startUpdatePolling(0);
  }
}

async function installAvailableUpdate() {
  const result = updateResult.value;
  if (!result?.update_available || !result.install_supported || updateBusy.value) return;
  startingUpdate.value = true;
  updateError.value = "";
  waitingForRestart.value = false;
  let pollingStarted = false;

  try {
    const currentStatus = await dashboardApi.getUpdateStatus();
    if (updateDisposed) return;
    if (isUpdatePhaseBusy(currentStatus.phase)) {
      rememberUpdateTarget(result.latest_version);
      startingUpdate.value = false;
      updateStatus.value = currentStatus;
      startUpdatePolling(0);
      return;
    }

    const target = rememberUpdateTarget(result.latest_version);
    if (!target) {
      failUpdate(t("升级未完成，请重试。"));
      return;
    }
    updateStatus.value = updateStatusFallback("checking");
    const generation = startUpdatePolling();
    pollingStarted = true;

    const status = await dashboardApi.installUpdate(result.latest_version);
    if (!isActiveUpdateGeneration(generation)) return;
    startingUpdate.value = false;
    acceptObservedUpdateStatus(status);
  } catch (error) {
    if (updateDisposed) return;
    startingUpdate.value = false;
    const failureDecision = decideInstallRequestFailure(
      error instanceof DashboardRequestError ? error.status : null,
      Boolean(updateTargetVersion.value),
    );
    if (failureDecision === "observe") {
      updateStatus.value = updateStatusFallback("checking");
      if (!pollingStarted) startUpdatePolling(0);
      return;
    }
    if (failureDecision === "fail") {
      failUpdate(error instanceof Error ? error.message : String(error));
      return;
    }
    // A network error can mean the accepted installer has already stopped the
    // old process before fetch received its response.
    waitingForRestart.value = true;
    updateStatus.value = updateStatusFallback("installing");
    if (!pollingStarted) startUpdatePolling(0);
  }
}

onMounted(() => {
  updateDisposed = false;
  void loadSettings();
  void restoreUpdateState();
});
onActivated(() => {
  if (saving.value || testingProxy.value) return;
  if (settingsStore.settings && !revalidateGate.shouldRun()) return;
  revalidateGate.record();
  if (savedConfig.value) {
    pendingSettingsMerge = {
      current: { ...config.value },
      saved: { ...savedConfig.value },
    };
  }
  void loadSettings();
});
onUnmounted(() => {
  updateDisposed = true;
  cancelUpdatePolling();
});
</script>

<style scoped>
.settings-grid {
  display: grid;
  grid-template-columns: minmax(0, 1fr);
  gap: var(--ocg-space-lg);
  max-width: 1080px;
  margin: 0 auto;
}
.settings-card {
  padding: 22px;
  border: 1px solid var(--ocg-border);
  border-radius: var(--ocg-radius-lg);
  background: var(--ocg-surface);
  box-shadow: var(--ocg-shadow-sm);
}
.settings-side {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  align-self: start;
  gap: var(--ocg-space-lg);
}
.downstream-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  align-items: start;
  gap: var(--ocg-space-lg);
  padding-top: 18px;
  border-top: 1px solid var(--ocg-border);
}
.settings-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  margin-bottom: 18px;
}
.settings-head h2 {
  margin: 0;
  color: var(--ocg-ink);
  font: 700 var(--ocg-font-lg)/1.3 "Bahnschrift", "Segoe UI Variable Display", sans-serif;
}
.settings-head p {
  margin: var(--ocg-space-xs) 0 0;
  color: var(--ocg-subtle);
  font-size: var(--ocg-font-sm);
}
.section-icon {
  margin-right: 6px;
  vertical-align: -0.15em;
}
.client-root-field,
.gateway-port-field {
  width: 100%;
}
.client-root-field > p,
.gateway-port-field > p {
  margin: 6px 0 0;
  color: var(--ocg-subtle);
  font-size: var(--ocg-font-xs);
  line-height: 1.5;
}
.settings-subsection {
  margin-top: var(--ocg-space-sm);
  padding-top: 18px;
  border-top: 1px solid var(--ocg-border);
}
.settings-subsection h3 {
  margin: 0;
  color: var(--ocg-ink);
  font: 700 var(--ocg-font-lg)/1.3 "Bahnschrift", "Segoe UI Variable Display", sans-serif;
}
.proxy-mode-group {
  display: flex;
  flex-wrap: wrap;
  gap: var(--ocg-space-sm) 18px;
  width: 100%;
}
.proxy-mode-help {
  min-height: 1.4em;
  margin: var(--ocg-space-sm) 0 var(--ocg-space-md);
}
.proxy-test-row {
  display: flex;
  align-items: center;
  gap: 10px;
}
.proxy-test-result {
  margin-top: var(--ocg-space-md);
}
.proxy-direction-group {
  display: flex;
  flex-wrap: wrap;
  gap: var(--ocg-space-xs) var(--ocg-space-lg);
}
.proxy-model-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
  gap: 6px var(--ocg-space-lg);
  width: 100%;
}
.proxy-model-option {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 0 var(--ocg-space-sm);
}
.proxy-model-hint {
  color: var(--n-text-color-disabled, inherit);
  font-size: 12px;
}
.proxy-model-free-hint {
  flex-basis: 100%;
  padding-left: var(--ocg-space-xl);
  color: var(--n-text-color-warning, inherit);
  font-size: 12px;
}
.proxy-stale-note {
  margin-top: var(--ocg-space-sm);
}
.settings-load-error {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--ocg-space-md);
  margin-bottom: var(--ocg-space-md);
}
.timeout-field {
  display: flex;
  flex-direction: column;
  gap: var(--ocg-space-xs);
  width: 100%;
}
.field-caption {
  font-size: var(--ocg-font-xs);
  color: var(--ocg-subtle);
  line-height: 1.4;
}
.theme-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(72px, 1fr));
  gap: var(--ocg-space-sm);
}
.theme-option {
  position: relative;
  display: flex;
  min-width: 0;
  min-height: 64px;
  align-items: center;
  justify-content: center;
  gap: var(--ocg-space-sm);
  padding: var(--ocg-space-sm);
  border: 1px solid var(--ocg-border);
  border-radius: var(--ocg-radius-md);
  color: var(--ocg-muted);
  background: var(--ocg-canvas);
  font: 600 var(--ocg-font-sm)/1 "Segoe UI Variable Text", "Microsoft YaHei UI", sans-serif;
  cursor: pointer;
  transition: border-color 0.16s ease, box-shadow 0.16s ease, color 0.16s ease;
}
.theme-option:hover {
  border-color: var(--ocg-primary);
  color: var(--ocg-ink);
}
.theme-option:focus-visible {
  outline: 2px solid var(--ocg-primary);
  outline-offset: 2px;
}
.theme-option--selected {
  border-color: var(--ocg-primary);
  color: var(--ocg-primary);
  box-shadow: 0 0 0 2px var(--ocg-primary);
}
.theme-swatch {
  width: 20px;
  height: 20px;
  flex: 0 0 20px;
  border-radius: 50%;
  box-shadow: inset 0 0 0 1px rgb(0 0 0 / 12%);
}
.theme-swatch--default {
  background: linear-gradient(135deg, #fff 0 50%, #000 50%) !important;
  box-shadow: inset 0 0 0 1px #8c8994;
}
.theme-swatch--white {
  box-shadow: inset 0 0 0 1px #8c8994;
}
.theme-check {
  position: absolute;
  top: 5px;
  right: 5px;
  font-size: var(--ocg-font-xs);
}
.update-result {
  margin-top: 14px;
}
.update-result:empty {
  margin-top: 0;
}
.update-result-content {
  display: grid;
  justify-items: start;
  gap: var(--ocg-space-md);
}
.update-actions {
  display: flex;
  flex-wrap: wrap;
  gap: var(--ocg-space-sm);
}
.update-status-body {
  display: grid;
  gap: 10px;
}
.update-status-body p {
  margin: 0;
}
.update-versions {
  display: grid;
  gap: 6px;
  margin: 0;
}
.update-versions > div {
  display: grid;
  grid-template-columns: auto 1fr;
  align-items: baseline;
  gap: 10px;
}
.update-versions dt {
  color: var(--ocg-subtle);
  font-size: var(--ocg-font-xs);
}
.update-versions dd {
  margin: 0;
}
.update-result-content code {
  color: var(--ocg-ink);
  font-family: "Cascadia Mono", Consolas, monospace;
  font-size: var(--ocg-font-md);
  font-weight: 600;
  line-height: 1.4;
}

@media (max-width: 800px) {
  .settings-side,
  .downstream-grid {
    grid-template-columns: 1fr;
  }
}
</style>
