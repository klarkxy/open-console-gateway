<template>
  <n-config-provider
    class="app-provider"
    :theme="naiveTheme"
    :theme-overrides="themeOverrides"
    :locale="naiveLocale"
    :date-locale="naiveDateLocale"
  >
    <n-global-style />

    <main v-if="authState !== 'ready'" class="auth-page">
      <section class="auth-panel">
        <div class="auth-panel-head">
          <div class="auth-brand"><span>Open</span> Console Gateway</div>
          <LocaleSwitcher />
        </div>
        <h1>{{ authState === "register" ? t("创建管理员") : t("管理员登录") }}</h1>
        <p v-if="authState === 'checking'" class="auth-copy">{{ t("正在连接管理服务…") }}</p>
        <n-form
          v-else
          class="auth-form"
          :model="authFormModel"
          label-placement="top"
          :show-feedback="false"
          @submit.prevent="submitAuth"
        >
          <n-form-item :label="t('用户名')">
            <n-input
              v-model:value="authUsername"
              :input-props="{ 'aria-label': t('用户名') }"
              autocomplete="username"
              placeholder="admin"
              autofocus
            />
          </n-form-item>
          <n-form-item :label="t('密码')">
            <n-input
              v-model:value="authPassword"
              :input-props="{ 'aria-label': t('密码') }"
              type="password"
              :autocomplete="authState === 'register' ? 'new-password' : 'current-password'"
              :placeholder="t('至少 8 个字符')"
              show-password-on="click"
            />
          </n-form-item>
          <n-form-item v-if="authState === 'register'" :label="t('确认密码')">
            <n-input
              v-model:value="authPasswordConfirm"
              :input-props="{ 'aria-label': t('确认密码') }"
              type="password"
              autocomplete="new-password"
              :placeholder="t('再次输入密码')"
              show-password-on="click"
            />
          </n-form-item>
          <p v-if="authError" class="auth-error" role="alert">{{ authError }}</p>
          <n-button
            attr-type="submit"
            type="primary"
            block
            :disabled="!authUsername.trim() || !authPassword"
          >
            {{ authState === "register" ? t("创建并进入") : t("登录") }}
          </n-button>
        </n-form>
      </section>
      <img :src="characterImage" alt="" class="auth-character" aria-hidden="true" />
    </main>

    <n-message-provider v-else>
      <n-dialog-provider>
      <BrowserSession
        v-if="activeKey === 'browser'"
        :session-token="browserSessionToken"
      />
      <n-layout v-else has-sider class="app-shell">
        <n-layout-sider
          bordered
          collapse-mode="width"
          :collapsed-width="64"
          :width="208"
          :collapsed="collapsed"
          show-trigger
          class="app-sider"
          :class="{ 'app-sider--collapsed': collapsed }"
          @collapse="collapsed = true"
          @expand="collapsed = false"
        >
          <div class="brand" :class="{ collapsed }" aria-label="Open Console Gateway">
            <span v-if="collapsed" class="brand-mark">OCG</span>
            <span v-else class="brand-name"><span class="brand-name-primary">Open Console</span><br />Gateway</span>
          </div>
          <n-menu
            :collapsed="collapsed"
            :collapsed-width="64"
            :collapsed-icon-size="22"
            :options="menuOptions"
            :value="activeKey"
            @update:value="(key: string) => selectView(key)"
          />
        </n-layout-sider>

        <n-layout class="app-main">
          <n-layout-header bordered class="app-header">
            <div class="desktop-title">{{ currentTitle }}</div>
            <div class="mobile-nav">
              <span class="brand-mark" role="img" aria-label="Open Console Gateway">OCG</span>
              <n-dropdown
                class="mobile-nav-dropdown"
                trigger="click"
                :keyboard="true"
                :show="mobileMenuShown"
                :options="mobileMenuOptions"
                @select="selectMobileView"
                @update:show="mobileMenuShown = $event"
              >
                <n-button
                  quaternary
                  class="mobile-nav-trigger"
                  aria-haspopup="menu"
                  :aria-expanded="mobileMenuShown"
                  :aria-label="currentTitle"
                >
                  {{ currentTitle }}
                </n-button>
              </n-dropdown>
            </div>
            <div class="header-actions">
              <LocaleSwitcher />
              <n-tooltip trigger="hover" :disabled="themeMenuShown">
                <template #trigger>
                  <n-dropdown
                    trigger="click"
                    :keyboard="false"
                    :show="themeMenuShown"
                    :options="themeMenuOptions"
                    :menu-props="themeMenuProps"
                    @select="selectTheme"
                    @update:show="updateThemeMenuShown"
                  >
                    <n-button
                      circle
                      quaternary
                      aria-controls="theme-menu"
                      aria-haspopup="menu"
                      :aria-expanded="themeMenuShown"
                      :aria-label="t('主题：{theme}', { theme: themeLabel })"
                      @keydown.esc.prevent.stop="closeThemeMenu"
                    >
                      <template #icon><n-icon :component="BgColorsOutlined" /></template>
                    </n-button>
                  </n-dropdown>
                </template>
                {{ t("主题：{theme}", { theme: themeLabel }) }}
              </n-tooltip>
              <n-tooltip v-if="!localMode" trigger="hover">
                <template #trigger>
                  <n-button
                    circle
                    quaternary
                    :aria-label="t('退出登录')"
                    :loading="loggingOut"
                    :disabled="loggingOut"
                    @click="logout"
                  >
                    <template #icon><n-icon :component="LogoutOutlined" /></template>
                  </n-button>
                </template>
                {{ t("退出登录") }}
              </n-tooltip>
            </div>
          </n-layout-header>

          <main class="app-content">
            <n-alert
              v-if="logoutError"
              class="app-error"
              type="error"
              closable
              @close="logoutError = ''"
            >
              {{ logoutError }}
            </n-alert>
            <n-alert
              v-if="upgradeGuidance"
              class="app-error"
              type="warning"
              closable
              @close="upgradeGuidance = ''"
            >
              {{ upgradeGuidance }}
            </n-alert>
            <!-- All views are kept alive so switching tabs preserves state
                 (scroll, filters, drafts); each view refreshes stale data in
                 its own onActivated hook. -->
            <KeepAlive>
              <Dashboard v-if="activeKey === 'dashboard'" @navigate="selectView" />
              <Keys v-else-if="activeKey === 'keys'" />
              <Accounts v-else-if="activeKey === 'accounts'" />
              <Providers v-else-if="activeKey === 'providers'" />
              <Aliases v-else-if="activeKey === 'aliases'" />
              <Applications v-else-if="activeKey === 'applications'" />
              <Logs v-else-if="activeKey === 'logs'" />
              <Settings
                v-else-if="activeKey === 'settings'"
                :theme-name="themeName"
                :resolved-theme="resolvedTheme"
                @update:theme-name="themeName = $event"
              />
              <Cpa v-else-if="activeKey === 'cpa'" />
            </KeepAlive>
          </main>
        </n-layout>
      </n-layout>
      </n-dialog-provider>
    </n-message-provider>
  </n-config-provider>
</template>

<script setup lang="ts">
import { computed, defineAsyncComponent, h, nextTick, onMounted, onUnmounted, ref, watch } from "vue";
import type { Component } from "vue";
import {
  NAlert,
  NButton,
  NConfigProvider,
  NDialogProvider,
  NDropdown,
  NForm,
  NFormItem,
  NGlobalStyle,
  NIcon,
  NInput,
  NLayout,
  NLayoutHeader,
  NLayoutSider,
  NMenu,
  NMessageProvider,
  NTooltip,
  darkTheme,
  useOsTheme,
} from "naive-ui";
import type { DropdownMenuProps, DropdownOption, MenuOption } from "naive-ui";
import {
  ApiOutlined,
  AppstoreOutlined,
  BgColorsOutlined,
  CheckOutlined,
  DashboardOutlined,
  CloudServerOutlined,
  FileTextOutlined,
  KeyOutlined,
  LinkOutlined,
  LogoutOutlined,
  SettingOutlined,
  TeamOutlined,
} from "@vicons/antd";
import LocaleSwitcher from "./components/LocaleSwitcher.vue";
import { locale, naiveDateLocale, naiveLocale, t } from "./i18n/index.ts";
import type { MessageKey } from "./i18n/index.ts";
import {
  DASHBOARD_AUTH_REQUIRED_EVENT,
  DASHBOARD_GONE_EVENT,
  DashboardRequestError,
} from "./api/dashboard";
import { useSessionStore } from "./stores/session.ts";
import {
  applyTheme,
  getThemeStorage,
  getThemeTokens,
  readTheme,
  resolveTheme,
  THEME_OPTIONS,
  toNaiveThemeOverrides,
  writeTheme,
} from "./theme";
import type { ThemeName } from "./theme";
import { userFacingError } from "./utils/errors.ts";
import {
  APP_NAVIGATION,
  APP_NAVIGATION_GROUPS,
  CORE_APP_NAVIGATION,
  EXTENSION_APP_NAVIGATION,
  applyAppViewSearchParams,
  isLegacyPricingView,
  resolveAppViewKey,
  type AppNavigationItem,
  type AppViewKey,
  type ProviderScopeQuery,
} from "./views/app-navigation.ts";

type ViewKey = AppViewKey;

const Dashboard = defineAsyncComponent(() => import("./views/Dashboard.vue"));
const Keys = defineAsyncComponent(() => import("./views/Keys.vue"));
const Accounts = defineAsyncComponent(() => import("./views/Accounts.vue"));
const Providers = defineAsyncComponent(() => import("./views/Providers.vue"));
const Aliases = defineAsyncComponent(() => import("./views/Aliases.vue"));
const Applications = defineAsyncComponent(() => import("./views/Applications.vue"));
const Logs = defineAsyncComponent(() => import("./views/Logs.vue"));
const Settings = defineAsyncComponent(() => import("./views/Settings.vue"));
const Cpa = defineAsyncComponent(() => import("./views/Cpa.vue"));
const BrowserSession = defineAsyncComponent(() => import("./views/BrowserSession.vue"));

const osTheme = useOsTheme();
const collapsed = ref(false);
const activeKey = ref<ViewKey>(readView());
const themeStorage = getThemeStorage();
const themeName = ref<ThemeName>(readTheme(themeStorage));
const themeMenuShown = ref(false);
const mobileMenuShown = ref(false);
const characterImage = new URL("../assets/opencode-mascot.png", import.meta.url).href;
const authUsername = ref("");
const authPassword = ref("");
const authPasswordConfirm = ref("");
const authError = ref("");
const authState = ref<"checking" | "login" | "register" | "ready">("checking");
const localMode = ref(false);
const loggingOut = ref(false);
const logoutError = ref("");
const upgradeGuidance = ref("");
const session = useSessionStore();
const browserSessionToken = ref(new URLSearchParams(window.location.hash.slice(1)).get("session") ?? "");
if (browserSessionToken.value) {
  const sanitizedBrowserUrl = new URL(window.location.href);
  sanitizedBrowserUrl.hash = "";
  window.history.replaceState(null, "", sanitizedBrowserUrl);
}
let suppressAuthRequired = false;

const authFormModel = computed(() => ({
  username: authUsername.value,
  password: authPassword.value,
  passwordConfirm: authPasswordConfirm.value,
}));

const resolvedTheme = computed(() => resolveTheme(themeName.value, osTheme.value));
const themeTokens = computed(() => getThemeTokens(themeName.value, osTheme.value));
const naiveTheme = computed(() => resolvedTheme.value === "black" ? darkTheme : null);
const themeOverrides = computed(() => toNaiveThemeOverrides(themeTokens.value));

function renderIcon(icon: Component) {
  return () => h(icon);
}

const navigationIcons: Record<AppNavigationItem["icon"], Component> = {
  dashboard: DashboardOutlined,
  keys: KeyOutlined,
  accounts: TeamOutlined,
  providers: CloudServerOutlined,
  aliases: LinkOutlined,
  applications: AppstoreOutlined,
  logs: FileTextOutlined,
  settings: SettingOutlined,
  cpa: ApiOutlined,
};

function menuOption(item: AppNavigationItem): MenuOption {
  return {
    label: t(item.label),
    key: item.key,
    icon: renderIcon(navigationIcons[item.icon]),
  };
}

function mobileMenuOption(item: AppNavigationItem): DropdownOption {
  return {
    ...menuOption(item),
    props: {
      role: "menuitemradio",
      "aria-checked": item.key === activeKey.value ? "true" : "false",
    },
  };
}

const menuOptions = computed<MenuOption[]>(() => {
  const options: MenuOption[] = CORE_APP_NAVIGATION.map(menuOption);
  if (EXTENSION_APP_NAVIGATION.length > 0) {
    options.push(
      { type: "divider", key: "extensions-divider" },
      {
        type: "group",
        key: "extensions",
        label: t(APP_NAVIGATION_GROUPS.extensions.label),
        children: EXTENSION_APP_NAVIGATION.map(menuOption),
      },
    );
  }
  return options;
});
const mobileMenuOptions = computed<DropdownOption[]>(() => {
  const options: DropdownOption[] = CORE_APP_NAVIGATION.map(mobileMenuOption);
  if (EXTENSION_APP_NAVIGATION.length > 0) {
    options.push(
      { type: "divider", key: "mobile-extensions-divider" },
      {
        label: t(APP_NAVIGATION_GROUPS.extensions.label),
        key: "mobile-extensions-label",
        disabled: true,
      },
      ...EXTENSION_APP_NAVIGATION.map(mobileMenuOption),
    );
  }
  return options;
});
const currentTitle = computed(() => t(
  APP_NAVIGATION.find(({ key }) => key === activeKey.value)?.label ?? "远程浏览器",
));
const themeNames = new Set<ThemeName>(THEME_OPTIONS.map(({ value }) => value));
const themeLabel = computed(() => {
  const selected = t((THEME_OPTIONS.find(({ value }) => value === themeName.value)?.label ?? "默认") as MessageKey);
  if (themeName.value !== "default") return selected;
  const resolved = t((THEME_OPTIONS.find(({ value }) => value === resolvedTheme.value)?.label ?? "皓白") as MessageKey);
  return t("默认 · {theme}", { theme: resolved });
});
const themeMenuOptions = computed<DropdownOption[]>(() => THEME_OPTIONS.map((option) => ({
  key: option.value,
  label: t(option.label as MessageKey),
  icon: () => h("span", {
    "aria-hidden": "true",
    style: {
      display: "inline-block",
      width: "16px",
      height: "16px",
      borderRadius: "50%",
      background: option.swatch,
      boxShadow: "inset 0 0 0 1px rgba(128, 128, 140, 0.45)",
    },
  }),
  extra: themeName.value === option.value
    ? () => h(NIcon, { component: CheckOutlined, size: 14, "aria-hidden": true })
    : undefined,
  props: {
    id: `theme-menu-option-${option.value}`,
    role: "menuitemradio",
    tabindex: -1,
    "aria-checked": themeName.value === option.value ? "true" : "false",
    onKeydown: (event: KeyboardEvent) => handleThemeMenuKeydown(event, option.value),
  },
})));
const themeMenuProps: DropdownMenuProps = () => ({
  id: "theme-menu",
  role: "menu",
  "aria-label": t("选择主题"),
});

const pendingProviderScope = ref<ProviderScopeQuery | null | undefined>(undefined);

function readView(): ViewKey {
  const params = new URLSearchParams(window.location.search);
  const raw = params.get("view");
  if (isLegacyPricingView(raw)) {
    const url = applyAppViewSearchParams(new URL(window.location.href), "providers");
    window.history.replaceState(null, "", url);
    return "providers";
  }
  return resolveAppViewKey(raw);
}

function selectView(key: string, extras?: ProviderScopeQuery) {
  const view = resolveAppViewKey(key);
  pendingProviderScope.value = extras;
  if (extras && view === "providers") {
    window.history.replaceState(
      null,
      "",
      applyAppViewSearchParams(new URL(window.location.href), view, extras),
    );
  }
  activeKey.value = view;
}

function selectMobileView(key: string | number) {
  mobileMenuShown.value = false;
  selectView(String(key));
}

function syncView(view: ViewKey) {
  const extras = pendingProviderScope.value;
  pendingProviderScope.value = undefined;
  const url = applyAppViewSearchParams(
    new URL(window.location.href),
    view,
    view === "providers" ? extras : null,
  );
  window.history.replaceState(null, "", url);
}

function onPopState() {
  activeKey.value = readView();
}

function selectTheme(key: string | number) {
  if (typeof key === "string" && themeNames.has(key as ThemeName)) {
    themeName.value = key as ThemeName;
    if (themeMenuShown.value) {
      themeMenuShown.value = false;
      void nextTick(focusThemeTrigger);
    }
  }
}

async function updateThemeMenuShown(show: boolean) {
  themeMenuShown.value = show;
  if (!show) return;
  await nextTick();
  focusThemeMenuOption(themeName.value);
}

function focusThemeMenuOption(theme: ThemeName) {
  document.querySelector<HTMLElement>(`#theme-menu-option-${theme}`)?.focus();
}

function focusThemeTrigger() {
  document.querySelector<HTMLElement>('[aria-controls="theme-menu"]')?.focus();
}

function closeThemeMenu() {
  if (!themeMenuShown.value) return;
  themeMenuShown.value = false;
  void nextTick(focusThemeTrigger);
}

function closeOpenThemeMenuOnEscape(event: KeyboardEvent) {
  if (!themeMenuShown.value || event.key !== "Escape") return;
  event.preventDefault();
  closeThemeMenu();
}

function handleThemeMenuKeydown(event: KeyboardEvent, current: ThemeName) {
  const index = THEME_OPTIONS.findIndex(({ value }) => value === current);
  let nextIndex: number | undefined;
  if (event.key === "ArrowDown" || event.key === "ArrowRight") {
    nextIndex = (index + 1) % THEME_OPTIONS.length;
  } else if (event.key === "ArrowUp" || event.key === "ArrowLeft") {
    nextIndex = (index - 1 + THEME_OPTIONS.length) % THEME_OPTIONS.length;
  } else if (event.key === "Home") {
    nextIndex = 0;
  } else if (event.key === "End") {
    nextIndex = THEME_OPTIONS.length - 1;
  } else if (event.key === "Enter" || event.key === " ") {
    event.preventDefault();
    event.stopPropagation();
    selectTheme(current);
    return;
  } else if (event.key === "Escape") {
    event.preventDefault();
    event.stopPropagation();
    closeThemeMenu();
    return;
  } else {
    return;
  }
  event.preventDefault();
  event.stopPropagation();
  focusThemeMenuOption(THEME_OPTIONS[nextIndex].value);
}

function onAuthRequired(event: Event) {
  if (suppressAuthRequired) return;
  session.handleAuthRequired();
  logoutError.value = "";
  authState.value = "login";
  authPassword.value = "";
  authPasswordConfirm.value = "";
  authError.value = (event as CustomEvent<string>).detail || t("请重新登录");
}

function onDashboardGone(event: Event) {
  const detail = (event as CustomEvent<{ guidance?: string }>).detail;
  upgradeGuidance.value = detail?.guidance || "页面版本与服务不匹配，请刷新页面后重试；若仍失败请升级到最新版本";
}

async function loadAuthStatus() {
  authState.value = "checking";
  try {
    const status = await session.loadStatus();
    localMode.value = status.local;
    authError.value = "";
    logoutError.value = "";
    authState.value = status.authenticated ? "ready" : status.initialized ? "login" : "register";
    suppressAuthRequired = false;
  } catch (e) {
    authState.value = "login";
    authError.value = t("连接失败: {error}", {
      error: userFacingError(e, t("无法连接到本地服务，请确认程序正在运行后重试")),
    });
  }
}

async function submitAuth() {
  const mode = authState.value;
  const username = authUsername.value.trim();
  if (!username || !authPassword.value) return;
  if (mode === "register" && [...username].length > 64) {
    authError.value = t("用户名需为 1 至 64 个字符");
    return;
  }
  const passwordLength = [...authPassword.value].length;
  if (mode === "register" && (passwordLength < 8 || passwordLength > 256)) {
    authError.value = t("密码需为 8 至 256 个字符");
    return;
  }
  if (mode === "register" && authPassword.value !== authPasswordConfirm.value) {
    authError.value = t("两次输入的密码不一致");
    return;
  }
  authState.value = "checking";
  try {
    if (mode === "register") await session.register(username, authPassword.value);
    else await session.login(username, authPassword.value);
    authPassword.value = "";
    authPasswordConfirm.value = "";
    authError.value = "";
    logoutError.value = "";
    authState.value = "ready";
    suppressAuthRequired = false;
  } catch (e) {
    authPassword.value = "";
    authPasswordConfirm.value = "";
    let error = userFacingError(e, t("无法连接到本地服务，请确认程序正在运行后重试"));
    if (e instanceof DashboardRequestError) {
      if (mode === "login" && e.status === 401) error = t("用户名或密码错误");
      if (mode === "register" && e.status === 409) error = t("管理员已经创建，请直接登录");
    }
    if (mode === "login" && e instanceof DashboardRequestError && e.status === 401) {
      const status = await session.loadStatus().catch(() => null);
      if (status) {
        localMode.value = status.local;
        if (status.authenticated) {
          authError.value = "";
          logoutError.value = "";
          authState.value = "ready";
          suppressAuthRequired = false;
          return;
        }
        if (!status.initialized) {
          authError.value = "";
          authState.value = "register";
          return;
        }
      }
    }
    if (mode === "register") {
      const status = await session.loadStatus().catch(() => null);
      if (status?.initialized) {
        localMode.value = status.local;
        authError.value = error;
        authState.value = status.authenticated ? "ready" : "login";
        return;
      }
    }
    authError.value = error;
    authState.value = mode;
  }
}

async function logout() {
  if (loggingOut.value) return;
  loggingOut.value = true;
  logoutError.value = "";
  suppressAuthRequired = true;
  try {
    await session.logout();
    authPassword.value = "";
    authPasswordConfirm.value = "";
    authError.value = "";
    authState.value = "login";
  } catch (e) {
    suppressAuthRequired = false;
    const error = userFacingError(e, t("无法连接到本地服务，请确认程序正在运行后重试"));
    logoutError.value = t("退出登录失败: {error}", { error });
  } finally {
    loggingOut.value = false;
  }
}

watch(activeKey, syncView);
watch(locale, () => { authError.value = ""; });
watch(themeName, (value) => writeTheme(themeStorage, value));
watch([resolvedTheme, themeTokens], ([resolved, tokens]) => {
  applyTheme(document.documentElement, resolved, tokens);
}, { immediate: true });

onMounted(() => {
  window.addEventListener(DASHBOARD_AUTH_REQUIRED_EVENT, onAuthRequired);
  window.addEventListener(DASHBOARD_GONE_EVENT, onDashboardGone);
  window.addEventListener("popstate", onPopState);
  document.addEventListener("keydown", closeOpenThemeMenuOnEscape);
  void loadAuthStatus();
});

onUnmounted(() => {
  window.removeEventListener(DASHBOARD_AUTH_REQUIRED_EVENT, onAuthRequired);
  window.removeEventListener(DASHBOARD_GONE_EVENT, onDashboardGone);
  window.removeEventListener("popstate", onPopState);
  document.removeEventListener("keydown", closeOpenThemeMenuOnEscape);
});
</script>

<style scoped>
.app-provider,
.app-shell,
.app-main {
  height: 100%;
}

.auth-page {
  position: relative;
  min-height: 100%;
  overflow: hidden;
  display: flex;
  align-items: center;
  padding: clamp(24px, 7vw, 96px);
  background:
    radial-gradient(circle at 22% 18%, var(--ocg-primary-soft), transparent 34%),
    radial-gradient(circle at 82% 52%, var(--ocg-mascot-halo), transparent 34%),
    var(--ocg-canvas);
}
.auth-panel {
  position: relative;
  z-index: 2;
  width: min(408px, 100%);
  padding: 32px;
  border: 1px solid var(--ocg-border);
  border-radius: 14px;
  background: var(--ocg-surface);
  box-shadow: var(--ocg-shadow-lg);
}
.auth-brand,
.brand-name,
.desktop-title {
  font-family: "Bahnschrift", "Segoe UI Variable Display", sans-serif;
}
.auth-brand {
  font-size: var(--ocg-font-xl);
  font-weight: 700;
}
.auth-panel-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
}
.auth-brand span,
.brand-mark {
  color: var(--ocg-primary);
}
.auth-panel h1 {
  margin: 20px 0 22px;
  font-family: "Bahnschrift", "Segoe UI Variable Display", sans-serif;
  font-size: var(--ocg-font-xl);
}
.auth-copy {
  color: var(--ocg-muted);
}
.auth-form :deep(.n-form-item) {
  margin-bottom: 12px;
}
.auth-error {
  margin: 0 0 12px;
  color: var(--ocg-error);
  font-size: var(--ocg-font-sm);
}
.auth-character {
  position: absolute;
  right: clamp(-28px, 4vw, 92px);
  bottom: -54px;
  height: min(94vh, 980px);
  max-width: 60vw;
  object-fit: contain;
  filter:
    drop-shadow(0 0 1px var(--ocg-mascot-rim))
    drop-shadow(0 24px 30px rgba(27, 23, 52, 0.16));
  pointer-events: none;
  user-select: none;
}

.app-sider {
  background: var(--ocg-surface);
}
.brand {
  display: flex;
  align-items: center;
  gap: 8px;
  height: 58px;
  padding: 0 18px;
  border-bottom: 1px solid var(--ocg-border);
  overflow: hidden;
}
.brand.collapsed {
  justify-content: center;
  padding: 0;
}
.brand-mark {
  flex: 0 0 auto;
  font: 800 16px/1 "Bahnschrift", "Segoe UI Variable Display", sans-serif;
  letter-spacing: 0.04em;
}
.brand-name {
  color: var(--ocg-ink);
  font-size: var(--ocg-font-md);
  font-weight: 650;
  line-height: 1.25;
  min-width: 0;
}
.brand-name-primary {
  color: var(--ocg-primary);
}
.app-header {
  height: 58px;
  padding: 0 20px;
  display: flex;
  align-items: center;
  justify-content: space-between;
  background: color-mix(in srgb, var(--ocg-surface) 94%, transparent);
}
.desktop-title {
  color: var(--ocg-ink);
  font-size: var(--ocg-font-lg);
  font-weight: 650;
}
.header-actions {
  display: flex;
  align-items: center;
  gap: 4px;
}
.mobile-nav {
  display: none;
  min-width: 0;
  align-items: center;
  gap: 8px;
}
.app-content {
  height: calc(100% - 58px);
  min-width: 0;
  min-height: 0;
  padding: 24px;
  overflow-y: auto;
  background: var(--ocg-canvas);
}
.app-error {
  margin-bottom: 16px;
}

@media (max-width: 1023px) {
  .app-sider {
    display: none;
  }
  .desktop-title {
    display: none;
  }
  .mobile-nav {
    display: flex;
    flex: 1 1 auto;
  }
  .mobile-nav-dropdown {
    min-width: 0;
  }
  .mobile-nav-trigger {
    max-width: 100%;
  }
  .app-header {
    padding: 0 12px;
  }
  .app-content {
    padding: 16px;
  }
}

@media (max-width: 640px) {
  .auth-page {
    padding: 16px;
  }
  .auth-panel {
    padding: 24px 20px;
  }
  .auth-character {
    right: -190px;
    max-width: none;
    opacity: 0.14;
  }
  .mobile-nav > .brand-mark {
    display: none;
  }
  .app-content {
    padding: 12px;
  }
}
</style>
