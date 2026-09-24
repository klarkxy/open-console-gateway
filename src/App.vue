<template>
  <n-config-provider class="app-provider" :theme="naiveTheme" :theme-overrides="themeOverrides" :locale="naiveLocale" :date-locale="naiveDateLocale">
    <n-global-style />
    <main v-if="authState !== 'ready'" class="auth-page">
      <section class="auth-panel">
        <div class="auth-panel-head">
          <div class="auth-brand"><span class="brand-symbol" aria-hidden="true">O</span><span>Open Console Gateway</span></div>
          <LocaleSwitcher />
        </div>
        <h1>{{ authState === "register" ? t("创建管理员") : t("管理员登录") }}</h1>
        <p v-if="authState === 'checking'" class="auth-copy" role="status">{{ t("正在连接管理服务…") }}</p>
        <n-form v-else class="auth-form" :model="authFormModel" label-placement="top" :show-feedback="false" @submit.prevent="submitAuth">
          <n-form-item :label="t('用户名')">
            <n-input v-model:value="authUsername" :input-props="{ 'aria-label': t('用户名') }" autocomplete="username" placeholder="admin" autofocus />
          </n-form-item>
          <n-form-item :label="t('密码')">
            <n-input v-model:value="authPassword" :input-props="{ 'aria-label': t('密码') }" type="password" :autocomplete="authState === 'register' ? 'new-password' : 'current-password'" :placeholder="t('至少 8 个字符')" show-password-on="click" />
          </n-form-item>
          <n-form-item v-if="authState === 'register'" :label="t('确认密码')">
            <n-input v-model:value="authPasswordConfirm" :input-props="{ 'aria-label': t('确认密码') }" type="password" autocomplete="new-password" :placeholder="t('再次输入密码')" show-password-on="click" />
          </n-form-item>
          <p v-if="authError" class="auth-error" role="alert">{{ authError }}</p>
          <n-button attr-type="submit" type="primary" block :disabled="!authUsername.trim() || !authPassword">{{ authState === "register" ? t("创建并进入") : t("登录") }}</n-button>
        </n-form>
      </section>
      <img :src="characterImage" alt="" class="auth-character" aria-hidden="true" />
    </main>
    <n-message-provider v-else>
      <n-dialog-provider>
        <BrowserSession v-if="activeKey === 'browser'" :session-token="browserSessionToken" />
        <n-layout v-else has-sider class="app-shell">
          <n-layout-sider collapse-mode="width" :collapsed-width="64" :width="224" :collapsed="collapsed" show-trigger class="app-sider" :class="{ 'app-sider--collapsed': collapsed }" @collapse="collapsed = true" @expand="collapsed = false">
            <div class="brand" :class="{ collapsed }" aria-label="Open Console Gateway">
              <span class="brand-symbol" aria-hidden="true">O</span>
              <span v-if="!collapsed" class="brand-name"><span>Open Console</span><small>Gateway</small></span>
            </div>
            <nav aria-label="Open Console Gateway">
              <n-menu :collapsed="collapsed" :collapsed-width="64" :collapsed-icon-size="20" :options="menuOptions" :value="activeKey" @update:value="(key: string) => selectView(key)" />
            </nav>
          </n-layout-sider>
          <n-layout class="app-main">
            <n-layout-header class="app-header">
              <h1 class="desktop-title">{{ currentTitle }}</h1>
              <div class="mobile-nav">
                <span class="brand-symbol" role="img" aria-label="Open Console Gateway">O</span>
                <n-dropdown class="mobile-nav-dropdown" trigger="click" :keyboard="true" :show="mobileMenuShown" :options="mobileMenuOptions" @select="selectMobileView" @update:show="mobileMenuShown = $event">
                  <n-button quaternary class="mobile-nav-trigger" aria-haspopup="menu" :aria-expanded="mobileMenuShown" :aria-label="currentTitle">{{ currentTitle }}<span class="mobile-nav-chevron" aria-hidden="true">⌄</span></n-button>
                </n-dropdown>
              </div>
              <div class="header-actions">
                <AppCommandPalette :items="navigationItems" :active-key="activeKey" @select="selectView" />
                <span class="header-divider" aria-hidden="true" />
                <LocaleSwitcher />
                <ThemeSwitcher v-model:theme-name="themeName" :resolved-theme="resolvedTheme" />
                <n-tooltip v-if="!localMode" trigger="hover">
                  <template #trigger><n-button circle quaternary :aria-label="t('退出登录')" :loading="loggingOut" :disabled="loggingOut" @click="logout"><template #icon><n-icon :component="LogoutOutlined" /></template></n-button></template>
                  {{ t("退出登录") }}
                </n-tooltip>
              </div>
            </n-layout-header>
            <main class="app-content">
              <n-alert v-if="logoutError" class="app-error" type="error" closable @close="logoutError = ''">{{ logoutError }}</n-alert>
              <n-alert v-if="upgradeGuidance" class="app-error" type="warning" closable @close="upgradeGuidance = ''">{{ upgradeGuidance }}</n-alert>
              <!-- Keep views mounted across navigation: filters, scroll and drafts survive. -->
              <KeepAlive>
                <Dashboard v-if="activeKey === 'dashboard'" @navigate="selectView" />
                <Keys v-else-if="activeKey === 'keys'" />
                <Accounts v-else-if="activeKey === 'accounts'" />
                <Providers v-else-if="activeKey === 'providers'" />
                <Aliases v-else-if="activeKey === 'aliases'" />
                <Applications v-else-if="activeKey === 'applications'" />
                <Logs v-else-if="activeKey === 'logs'" />
                <Settings v-else-if="activeKey === 'settings'" :theme-name="themeName" :resolved-theme="resolvedTheme" @update:theme-name="themeName = $event" />
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
import { computed, defineAsyncComponent, h, onMounted, onUnmounted, ref, watch } from "vue";
import type { Component } from "vue";
import { NAlert, NButton, NConfigProvider, NDialogProvider, NDropdown, NForm, NFormItem, NGlobalStyle, NIcon, NInput, NLayout, NLayoutHeader, NLayoutSider, NMenu, NMessageProvider, NTooltip, darkTheme, useOsTheme } from "naive-ui";
import type { DropdownOption, MenuOption } from "naive-ui";
import { ApiOutlined, AppstoreOutlined, DashboardOutlined, CloudServerOutlined, FileTextOutlined, KeyOutlined, LinkOutlined, LogoutOutlined, SettingOutlined, TeamOutlined } from "@vicons/antd";
import LocaleSwitcher from "./components/LocaleSwitcher.vue";
import ThemeSwitcher from "./components/ThemeSwitcher.vue";
import AppCommandPalette from "./components/AppCommandPalette.vue";
import { readSidebarCollapsed, writeSidebarCollapsed } from "./domain/navigation-search.ts";
import { locale, naiveDateLocale, naiveLocale, t } from "./i18n/index.ts";
import { DASHBOARD_AUTH_REQUIRED_EVENT, DASHBOARD_GONE_EVENT, DashboardRequestError } from "./api/dashboard";
import { useSessionStore } from "./stores/session.ts";
import { applyTheme, getThemeStorage, getThemeTokens, readTheme, resolveTheme, toNaiveThemeOverrides, writeTheme } from "./theme";
import type { ThemeName } from "./theme";
import { userFacingError } from "./utils/errors.ts";
import { APP_NAVIGATION, APP_NAVIGATION_GROUPS, CORE_APP_NAVIGATION, EXTENSION_APP_NAVIGATION, applyAppViewSearchParams, isLegacyPricingView, resolveAppViewKey, type AppNavigationItem, type AppViewKey, type ProviderScopeQuery } from "./views/app-navigation.ts";

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
const themeStorage = getThemeStorage();
const collapsed = ref(readSidebarCollapsed(themeStorage));
const activeKey = ref<ViewKey>(readView());
const themeName = ref<ThemeName>(readTheme(themeStorage));
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
const authFormModel = computed(() => ({ username: authUsername.value, password: authPassword.value, passwordConfirm: authPasswordConfirm.value }));
const resolvedTheme = computed(() => resolveTheme(themeName.value, osTheme.value));
const themeTokens = computed(() => getThemeTokens(themeName.value, osTheme.value));
const naiveTheme = computed(() => resolvedTheme.value === "black" ? darkTheme : null);
const themeOverrides = computed(() => toNaiveThemeOverrides(themeTokens.value));
function renderIcon(icon: Component) { return () => h(icon); }
const navigationIcons: Record<AppNavigationItem["icon"], Component> = { dashboard: DashboardOutlined, keys: KeyOutlined, accounts: TeamOutlined, providers: CloudServerOutlined, aliases: LinkOutlined, applications: AppstoreOutlined, logs: FileTextOutlined, settings: SettingOutlined, cpa: ApiOutlined };
const navigationItems = computed(() => APP_NAVIGATION.map((item) => ({ key: item.key, label: t(item.label), icon: navigationIcons[item.icon] })));
function menuOption(item: AppNavigationItem): MenuOption { return { label: t(item.label), key: item.key, icon: renderIcon(navigationIcons[item.icon]) }; }
function mobileMenuOption(item: AppNavigationItem): DropdownOption {
  return { ...menuOption(item), props: { role: "menuitemradio", "aria-checked": item.key === activeKey.value ? "true" : "false" } };
}
const menuOptions = computed<MenuOption[]>(() => {
  const options: MenuOption[] = CORE_APP_NAVIGATION.map(menuOption);
  if (EXTENSION_APP_NAVIGATION.length > 0) options.push(
    { type: "divider", key: "extensions-divider" },
    { type: "group", key: "extensions", label: t(APP_NAVIGATION_GROUPS.extensions.label), children: EXTENSION_APP_NAVIGATION.map(menuOption) },
  );
  return options;
});
const mobileMenuOptions = computed<DropdownOption[]>(() => {
  const options: DropdownOption[] = CORE_APP_NAVIGATION.map(mobileMenuOption);
  if (EXTENSION_APP_NAVIGATION.length > 0) options.push(
    { type: "divider", key: "mobile-extensions-divider" },
    { label: t(APP_NAVIGATION_GROUPS.extensions.label), key: "mobile-extensions-label", disabled: true },
    ...EXTENSION_APP_NAVIGATION.map(mobileMenuOption),
  );
  return options;
});
const currentTitle = computed(() => t(APP_NAVIGATION.find(({ key }) => key === activeKey.value)?.label ?? "远程浏览器"));
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
  if (extras && view === "providers") window.history.replaceState(null, "", applyAppViewSearchParams(new URL(window.location.href), view, extras));
  activeKey.value = view;
}
function selectMobileView(key: string | number) { mobileMenuShown.value = false; selectView(String(key)); }
function syncView(view: ViewKey) {
  const extras = pendingProviderScope.value;
  pendingProviderScope.value = undefined;
  const url = applyAppViewSearchParams(new URL(window.location.href), view, view === "providers" ? extras : null);
  window.history.replaceState(null, "", url);
}
function onPopState() { activeKey.value = readView(); }
function onAuthRequired(event: Event) {
  if (suppressAuthRequired) return;
  session.handleAuthRequired();
  logoutError.value = "";
  authState.value = "login";
  authPassword.value = "";
  authPasswordConfirm.value = "";
  authError.value = (event as CustomEvent<string>).detail || t("重新登录");
}
function onDashboardGone(event: Event) {
  const detail = (event as CustomEvent<{ guidance?: string }>).detail;
  upgradeGuidance.value = detail?.guidance || t("页面版本与服务不匹配，刷新页面后重试；仍失败请升级到最新版本");
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
    authError.value = t("连接失败：{error}", { error: userFacingError(e, t("无法连接到本地服务，请确认程序正在运行后重试")) });
  }
}
async function submitAuth() {
  const mode = authState.value;
  const username = authUsername.value.trim();
  if (!username || !authPassword.value) return;
  if (mode === "register" && [...username].length > 64) { authError.value = t("用户名需为 1 至 64 个字符"); return; }
  const passwordLength = [...authPassword.value].length;
  if (mode === "register" && (passwordLength < 8 || passwordLength > 256)) { authError.value = t("密码需为 8 至 256 个字符"); return; }
  if (mode === "register" && authPassword.value !== authPasswordConfirm.value) { authError.value = t("两次输入的密码不一致"); return; }
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
      if (mode === "register" && e.status === 409) error = t("管理员已创建，直接登录");
    }
    if (mode === "login" && e instanceof DashboardRequestError && e.status === 401) {
      const status = await session.loadStatus().catch(() => null);
      if (status) {
        localMode.value = status.local;
        if (status.authenticated) { authError.value = ""; logoutError.value = ""; authState.value = "ready"; suppressAuthRequired = false; return; }
        if (!status.initialized) { authError.value = ""; authState.value = "register"; return; }
      }
    }
    if (mode === "register") {
      const status = await session.loadStatus().catch(() => null);
      if (status?.initialized) { localMode.value = status.local; authError.value = error; authState.value = status.authenticated ? "ready" : "login"; return; }
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
    logoutError.value = t("退出登录失败：{error}", { error });
  } finally { loggingOut.value = false; }
}
watch(activeKey, syncView);
watch(locale, () => { authError.value = ""; });
watch(collapsed, (value) => writeSidebarCollapsed(themeStorage, value));
watch(themeName, (value) => writeTheme(themeStorage, value));
watch([resolvedTheme, themeTokens], ([resolved, tokens]) => { applyTheme(document.documentElement, resolved, tokens); }, { immediate: true });
onMounted(() => {
  window.addEventListener(DASHBOARD_AUTH_REQUIRED_EVENT, onAuthRequired);
  window.addEventListener(DASHBOARD_GONE_EVENT, onDashboardGone);
  window.addEventListener("popstate", onPopState);
  void loadAuthStatus();
});
onUnmounted(() => {
  window.removeEventListener(DASHBOARD_AUTH_REQUIRED_EVENT, onAuthRequired);
  window.removeEventListener(DASHBOARD_GONE_EVENT, onDashboardGone);
  window.removeEventListener("popstate", onPopState);
});
</script>

<style scoped src="./styles/shell.css"></style>
