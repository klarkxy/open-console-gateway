<template>
  <section class="space-y-4 min-w-0" aria-label="VS Code Copilot">
    <div class="flex flex-wrap items-center justify-between gap-3">
      <div><h2 class="text-base font-semibold">Open Console Gateway</h2><p class="text-sm text-[var(--ocg-muted)]">{{ t("安装扩展，模型和上下文由 OCG 同步。") }}</p></div>
      <button class="copilot-button" :disabled="busy" @click="inspect">{{ t("刷新") }}</button>
    </div>
    <p v-if="store.loading && !view" role="status">{{ t("加载中…") }}</p>
    <p v-if="store.error || actionError" role="alert" class="text-[var(--ocg-error)] break-words">{{ actionError || store.error }}</p>
    <template v-if="view">
      <div class="flex flex-wrap gap-3 items-center"><strong>{{ t(COPILOT_STATUS_KEYS[view.status]) }}</strong><span v-if="view.extensionVersion" class="text-sm text-[var(--ocg-muted)]">{{ view.extensionVersion }}</span></div>
      <p v-if="view.detail" class="text-sm break-words">{{ view.detail }}</p>
      <p v-if="view.activationRequired" role="status">{{ t("打开或重新加载所选 VS Code Profile，完成连接后刷新。") }}</p>
      <p v-if="view.status === 'connected'" class="text-sm">{{ t("已导入 Key，可用模型 {count} 个；实际请求仍需在 VS Code 中验证。", { count: view.modelCount ?? 0 }) }}</p>
      <div v-if="view.metadataMissing.length" role="status" class="space-y-2">
        <p>{{ t("以下模型缺少上下文或输出限制，请在 OCG 模型能力中补齐一次。") }}</p>
        <p class="font-mono text-sm break-words">{{ view.metadataMissing.join(' · ') }}</p>
        <RouterLink :to="appViewRoute('aliases')" class="text-[var(--ocg-primary)] underline">{{ t("前往模型能力") }}</RouterLink>
      </div>
      <dl v-if="view.target.userDataDir" class="text-sm space-y-1">
        <div><dt class="inline text-[var(--ocg-muted)]">{{ t("安装目标") }}: </dt><dd class="inline">{{ selectedInstallationLabel }} · {{ view.target.profile ?? t("默认 Profile") }}</dd></div>
        <div><dt class="inline text-[var(--ocg-muted)]">{{ t("用户数据目录") }}: </dt><dd class="inline font-mono break-all">{{ view.target.userDataDir }}</dd></div>
        <div><dt class="inline text-[var(--ocg-muted)]">{{ t("扩展目录") }}: </dt><dd class="inline font-mono break-all">{{ view.target.extensionsDir }}</dd></div>
      </dl>
      <p v-if="!targetCurrent" role="status">{{ t("目标已修改，请先检查目标。") }}</p>
      <div class="flex flex-wrap gap-2">
        <button data-action="install" class="copilot-button copilot-primary" :disabled="!ready('install')" @click="mutate('install')">{{ view.installed ? t("更新或重新连接") : t("安装并连接") }}</button>
        <button v-if="view.installed" class="copilot-button" :disabled="!ready('disconnect')" @click="mutate('disconnect')">{{ t("断开连接") }}</button>
        <button v-if="view.installed" class="copilot-button" :disabled="!ready('uninstall')" @click="uninstallConfirm = true">{{ t("卸载扩展") }}</button>
      </div>
      <section v-if="uninstallConfirm" class="space-y-2 border rounded-lg p-3" aria-live="polite">
        <p>{{ t("卸载前会请求 VS Code 清除本扩展的 Key。若等待激活，请打开所选 Profile，再刷新并继续卸载。OCG 中的 Key 保留。") }}</p>
        <button class="copilot-button" :disabled="!ready('uninstall')" @click="mutate('uninstall')">{{ t("确认卸载") }}</button>
        <button class="copilot-button ml-2" :disabled="store.mutating" @click="uninstallConfirm = false">{{ t("取消") }}</button>
      </section>
    </template>
    <details class="space-y-3" @toggle="uninstallConfirm = false">
      <summary class="cursor-pointer text-sm">{{ t("其他安装目标") }}</summary>
      <fieldset :disabled="busy" class="grid gap-3 sm:grid-cols-2 pt-3 border-0 px-0 pb-0 m-0 min-w-0">
        <label class="grid gap-1 text-sm">{{ t("VS Code 安装") }}<select :value="installation" @change="setDraft('installation', $event)" class="copilot-input"><option value="">{{ t("自动检测") }}</option><option v-for="i in view?.discoveredInstallations ?? []" :key="i.id" :value="i.id">{{ i.label }}</option></select></label>
        <label class="grid gap-1 text-sm">Profile<input :value="profile" @input="setDraft('profile', $event)" @compositionend="setDraft('profile', $event)" class="copilot-input" :placeholder="t('默认 Profile')" /></label>
        <label class="grid gap-1 text-sm">{{ t("用户数据目录") }}<input :value="userDataDir" @input="setDraft('userDataDir', $event)" @compositionend="setDraft('userDataDir', $event)" class="copilot-input" :placeholder="t('使用安装默认目录')" /></label>
        <label class="grid gap-1 text-sm">{{ t("扩展目录") }}<input :value="extensionsDir" @input="setDraft('extensionsDir', $event)" @compositionend="setDraft('extensionsDir', $event)" class="copilot-input" :placeholder="t('使用安装默认目录')" /></label>
      </fieldset>
      <button class="copilot-button mt-3" :disabled="busy" @click="inspect">{{ t("检查目标") }}</button>
    </details>
    <div class="border-t border-[var(--ocg-divider)] pt-3 space-y-2">
      <button class="copilot-button" :disabled="downloading" @click="download">{{ t("下载 VSIX 手动安装") }}</button>
      <p class="text-sm text-[var(--ocg-muted)]">{{ t("手动安装后运行 OCG: Connect，输入 OCG 地址和 Key。") }}</p>
    </div>
    <details data-section="legacy" class="border-t border-[var(--ocg-divider)] pt-3" @toggle="toggleLegacy">
      <summary class="cursor-pointer text-sm">{{ t("旧 JSON 配置与迁移") }}</summary>
      <p class="my-3 text-sm text-[var(--ocg-muted)]">{{ t("扩展连接成功后，可在下方移除旧 OCG JSON 配置。移除会检查归属并保留其他供应商。") }}</p>
      <ByokApplicationPanel v-if="legacyOpen" client="copilot" />
    </details>
  </section>
</template>
<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { RouterLink } from "vue-router";
import { t } from "../../i18n/index.ts";
import { useCopilotApplicationStore } from "../../stores/copilotApplication.ts";
import { useConnectionStore } from "../../stores/connection.ts";
import { copilotApplicationApi, type CopilotTarget } from "../../api/copilot-application.ts";
import { COPILOT_STATUS_KEYS, copilotActionReady, copilotTargetKey } from "../../domain/copilot-application.ts";
import { appViewRoute } from "../../views/app-navigation.ts";
import { dashboardErrorDetail } from "../../utils/errors.ts";
import ByokApplicationPanel from "./ByokApplicationPanel.vue";
const store = useCopilotApplicationStore(), connection = useConnectionStore();
const view = computed(() => store.application);
const cachedDraft: Array<string | null> = store.snapshotKey ? JSON.parse(store.snapshotKey) as Array<string | null> : [];
const installation = ref(cachedDraft[0] ?? ""), profile = ref(cachedDraft[1] ?? ""), userDataDir = ref(cachedDraft[2] ?? ""), extensionsDir = ref(cachedDraft[3] ?? "");
const actionError = ref(""), uninstallConfirm = ref(false), legacyOpen = ref(false), downloading = ref(false);
const target = computed<CopilotTarget>(() => ({ installation: installation.value || null, profile: profile.value.trim() || null, userDataDir: userDataDir.value.trim() || null, extensionsDir: extensionsDir.value.trim() || null }));
const targetCurrent = computed(() => store.snapshotKey === copilotTargetKey(target.value));
const selectedInstallationLabel = computed(() => { const current = view.value; return current?.discoveredInstallations.find(i => i.id === current.target.installation)?.label ?? current?.target.installation; });
function toggleLegacy(event: Event): void { legacyOpen.value = (event.target as HTMLDetailsElement | null)?.open ?? false; }
const busy = computed(() => store.loading || store.mutating);
function setDraft(field: "installation" | "profile" | "userDataDir" | "extensionsDir", event: Event): void {
  if ("isComposing" in event && event.isComposing) return;
  const value = (event.target as HTMLInputElement | HTMLSelectElement | null)?.value ?? "";
  ({ installation, profile, userDataDir, extensionsDir })[field].value = value;
}
function ready(action: "install" | "disconnect" | "uninstall"): boolean { return copilotActionReady(view.value, action, targetCurrent.value, busy.value); }
async function inspect(): Promise<void> { actionError.value = ""; uninstallConfirm.value = false; await store.inspect(target.value); }
async function mutate(action: "install" | "disconnect" | "uninstall"): Promise<void> {
  const current = view.value; if (!current?.fingerprint || !ready(action)) return;
  actionError.value = ""; const session = connection.currentSession();
  try {
    await store.mutate(action, { target: current.target, expectedFingerprint: current.fingerprint, ...(action === "install" ? { keyId: null } : {}) }, { expectedRevision: current.revision.revision, processGeneration: current.revision.processGeneration });
    if (session === connection.currentSession()) { uninstallConfirm.value = false; if (action === "install") void connection.reloadAfterMutation(session).catch(() => {}); }
  } catch (reason) { if (session === connection.currentSession()) actionError.value = dashboardErrorDetail(reason); }
}
async function download(): Promise<void> {
  downloading.value = true; actionError.value = ""; const session = connection.currentSession();
  try { const blob = await copilotApplicationApi.downloadPackage(); if (session !== connection.currentSession()) return; const url = URL.createObjectURL(blob); const a = document.createElement("a"); a.href = url; a.download = "open-console-gateway-copilot.vsix"; a.click(); setTimeout(() => URL.revokeObjectURL(url), 1000); }
  catch (reason) { if (session === connection.currentSession()) actionError.value = dashboardErrorDetail(reason); }
  finally { downloading.value = false; }
}
watch(target, () => { uninstallConfirm.value = false; actionError.value = ""; });
onMounted(() => { if (!view.value || !targetCurrent.value) void inspect(); });
</script>
<style scoped>
.copilot-button { padding: 6px 12px; border: 1px solid color-mix(in srgb, var(--ocg-muted) 68%, var(--ocg-surface)); border-radius: 6px; background: var(--ocg-surface); color: var(--ocg-ink); cursor: pointer; font: inherit; font-size: 13px; }
.copilot-button:disabled { opacity: .55; cursor: default; }
.copilot-button:focus-visible, .copilot-input:focus-visible { outline: 2px solid var(--ocg-primary); outline-offset: 2px; }
.copilot-primary { background: var(--ocg-primary); color: var(--ocg-on-primary); border-color: var(--ocg-primary); }
.copilot-input { width: 100%; min-width: 0; padding: 6px 8px; border: 1px solid color-mix(in srgb, var(--ocg-muted) 68%, var(--ocg-surface)); border-radius: 6px; color: var(--ocg-ink); background: var(--ocg-surface); }
</style>
