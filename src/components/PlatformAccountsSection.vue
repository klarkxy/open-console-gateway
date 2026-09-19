<template>
  <n-alert
    v-if="platformStore.error"
    type="error"
    :title="t('加载平台账号失败：{error}', { error: platformStore.error })"
  >
    <n-button size="small" secondary @click="reload">{{ t("重试") }}</n-button>
  </n-alert>

  <PlatformAccountFormModal
    :show="showForm"
    :editing="editingPlatform"
    :preset-kind="presetKind"
    :busy="platformStore.mutating"
    @update:show="showForm = $event"
    @save="onFormSave"
  />
  <PlatformLinkModal
    :show="showLink"
    :parent="linkParent"
    :candidates="linkCandidates"
    :busy="platformStore.mutating"
    @update:show="showLink = $event"
    @submit="onLinkSubmit"
    @add-key="onLinkModalAddKey"
  />
  <PlatformKeyFormModal
    :show="!!addKeyParent || !!editKeyAccount"
    :parent-name="keyFormParentName"
    :title="editKeyAccount ? t('编辑 Key') : t('添加 Key')"
    :editing="editKeyAccount ? { name: editKeyAccount.name, notes: editKeyAccount.notes } : null"
    :busy="platformStore.mutating"
    :external-error="addKeyError"
    @update:show="onKeyFormVisible"
    @save="onKeyFormSave"
  />
</template>

<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import {
  NAlert,
  NButton,
  useDialog,
  useMessage,
} from "naive-ui";
import { dashboardApi, DashboardRequestError, type Account } from "../api/dashboard.ts";
import { isRevisionConflict } from "../api/dashboard-v3.ts";
import {
  type PlatformAccount,
  type PlatformKind,
  type PlatformLink,
} from "../api/platform-accounts.ts";
import {
  PLATFORM_KEY_IMPORT_FAILURE_KEYS,
  canImportPlatformKeys,
  discoveredModelCapabilities,
  linkedAccountIdSet,
  platformHostedEndpoint,
  platformModelOverlay,
  type PlatformKeyImportFailureCode,
} from "../domain/platform-accounts.ts";
import { accountCapabilities } from "../domain/account-capabilities.ts";
import { t, type MessageKey } from "../i18n/index.ts";
import { usePlatformAccountsStore, type PlatformPersistOutcome } from "../stores/platformAccounts.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";
import PlatformAccountFormModal, {
  type PlatformAccountFormPayload,
} from "./PlatformAccountFormModal.vue";
import PlatformKeyFormModal, { type PlatformKeyFormPayload } from "./PlatformKeyFormModal.vue";
import PlatformLinkModal from "./PlatformLinkModal.vue";

const props = defineProps<{
  accounts: Account[];
}>();

const emit = defineEmits<{
  /** Server mutated account state (link/unlink rewrote the endpoint); reload the ordered list. */
  changed: [];
  /** Platform write committed, but the destination cards still show the prior snapshot. */
  destinationRefreshFailed: [error: string];
  /** A capabilities import returned the updated account; replace it in place. */
  accountUpdated: [account: Account];
}>();

const dialog = useDialog();
const message = useMessage();
const platformStore = usePlatformAccountsStore();

const showForm = ref(false);
const editingPlatform = ref<PlatformAccount | null>(null);
const presetKind = ref<PlatformKind>("new_api");

const showLink = ref(false);
const linkParent = ref<PlatformAccount | null>(null);

/**
 * Direct Add Key flow: the chosen parent instance is fixed for the whole
 * operation. After a known create success, `pendingLink` retains the returned
 * account id so a failed association retries ONLY the link — the create is
 * never repeated and the standalone Key survives.
 */
const addKeyParent = ref<PlatformAccount | null>(null);
const addKeyError = ref("");
const editKeyAccount = ref<Account | null>(null);

const keyFormParentName = computed(() => {
  if (addKeyParent.value) return addKeyParent.value.name;
  if (!editKeyAccount.value) return "";
  const link = platformStore.links.find((item) => item.accountId === editKeyAccount.value!.id);
  return platformStore.parents.find((parent) => parent.id === link?.platformAccountId)?.name ?? "";
});

const linkCandidates = computed(() => {
  const linked = linkedAccountIdSet(platformStore.links);
  return props.accounts.filter((account) => (
    accountCapabilities(account, null).endpointOnAccount && !linked.has(account.id)
  ));
});

watch(() => platformStore.error, (error) => {
  if (error) {
    message.error(t("加载平台账号失败：{error}", { error }));
  }
});

function notifyConflict(): void {
  message.warning(t("账号设置已被其他操作修改，已重新加载最新状态，请重试"));
  emit("changed");
}

async function reload(): Promise<void> {
  try {
    await platformStore.load();
  } catch {
    // Alert + error watch keep the same load-failure presentation.
  }
}

function mutationError(error: unknown, fallbackKey: MessageKey): void {
  message.error(t(fallbackKey, { error: dashboardErrorDetail(error) }));
}

function openCreate(kind: PlatformKind): void {
  editingPlatform.value = null;
  presetKind.value = kind;
  showForm.value = true;
}

function openEdit(parent: PlatformAccount): void {
  editingPlatform.value = parent;
  showForm.value = true;
}

/**
 * Single owner of the platform create/update write: the edit modal and the
 * Add Account chooser's embedded create form both funnel through here so
 * validation results, CAS conflict recovery, and the card reload match.
 */
async function persistPlatform(
  payload: PlatformAccountFormPayload,
  editing: PlatformAccount | null,
): Promise<PlatformPersistOutcome> {
  try {
    const outcome = await platformStore.createOrUpdate(payload, editing);
    if (outcome === "saved") {
      message.success(editing ? t("平台账号已更新") : t("平台账号已创建"));
      emit("changed");
    } else if (outcome === "saved_refresh_failed") {
      message.warning(t("已保存，但列表刷新失败。手动刷新，不要再次提交。"));
      emit("destinationRefreshFailed", platformStore.destinationRefreshError);
    } else if (outcome === "conflict") {
      notifyConflict();
    }
    return outcome;
  } catch (error) {
    mutationError(error, "保存失败：{error}");
    return "error";
  }
}

async function onFormSave(payload: PlatformAccountFormPayload): Promise<void> {
  const outcome = await persistPlatform(payload, editingPlatform.value);
  // A conflict already reloaded the world; keeping the stale modal open would
  // invite a second write against the old revision.
  if (outcome !== "error") showForm.value = false;
}

/** Add Account chooser entry point; true only when the create persisted. */
async function createPlatform(payload: PlatformAccountFormPayload): Promise<boolean> {
  const outcome = await persistPlatform(payload, null);
  return outcome === "saved" || outcome === "saved_refresh_failed";
}

function confirmDelete(parent: PlatformAccount): void {
  dialog.warning({
    title: t("删除平台账号"),
    content: t("确定删除平台账号 {name} 吗？其快照数据会一并删除，已保存的凭证不可恢复。", { name: parent.name }),
    positiveText: t("删除"),
    negativeText: t("取消"),
    onPositiveClick: () => deletePlatform(parent),
  });
}

async function deletePlatform(parent: PlatformAccount): Promise<void> {
  try {
    const outcome = await platformStore.remove(parent.id);
    if (outcome === "conflict") notifyConflict();
    else if (outcome === "ok") message.success(t("平台账号已删除"));
  } catch (error) {
    mutationError(error, "删除失败：{error}");
  }
}

async function importKeys(parent: PlatformAccount): Promise<void> {
  if (!canImportPlatformKeys(parent) || platformStore.mutating || platformStore.importing[parent.id]) return;
  dialog.info({
    title: t("从站点导入 Key"),
    content: t("将从站点拉取令牌并在本地创建 Key。已存在的 Key 会跳过。"),
    positiveText: t("导入"),
    negativeText: t("取消"),
    onPositiveClick: () => runImportKeys(parent),
  });
}

async function runImportKeys(parent: PlatformAccount): Promise<void> {
  try {
    const result = await platformStore.importKeys(parent.id);
    if (result === "error") return;
    if (result === "conflict") {
      notifyConflict();
      return;
    }
    emit("changed");
    if (result.imported === 0 && result.failed.length === 0) {
      message.info(t("没有可导入的 Key"));
      return;
    }
    const parts = [t("已导入 {imported} 把 Key", { imported: result.imported })];
    if (result.skippedExisting > 0) {
      parts.push(t("已跳过 {count} 把已存在的 Key", { count: result.skippedExisting }));
    }
    if (result.skippedDisabled > 0) {
      parts.push(t("{count} 把已停用", { count: result.skippedDisabled }));
    }
    if (result.failed.length > 0) {
      const sample = result.failed.slice(0, 3).map((item) => {
        const mapped = PLATFORM_KEY_IMPORT_FAILURE_KEYS[item.code as PlatformKeyImportFailureCode];
        return `${item.name}: ${mapped ? t(mapped) : item.code}`;
      }).join("；");
      parts.push(t("{count} 把导入失败", { count: result.failed.length }) + `（${sample}）`);
    }
    if (result.failed.length > 0 && result.imported === 0) message.warning(parts.join(" · "));
    else message.success(parts.join(" · "));
  } catch (error) {
    mutationError(error, "导入 Key 失败：{error}");
  }
}

async function refreshParent(parent: PlatformAccount): Promise<void> {
  try {
    const outcome = await platformStore.refreshParent(parent.id);
    if (outcome === "conflict") notifyConflict();
    else if (outcome === "ok") message.success(t("已刷新"));
  } catch (error) {
    mutationError(error, "刷新失败：{error}");
  }
}

async function refreshChild(parent: PlatformAccount, link: PlatformLink): Promise<void> {
  try {
    const outcome = await platformStore.refreshChild(parent.id, link.accountId);
    if (outcome === "conflict") notifyConflict();
    else if (outcome === "ok") message.success(t("已刷新"));
  } catch (error) {
    mutationError(error, "刷新失败：{error}");
  }
}

function openLink(parent: PlatformAccount): void {
  linkParent.value = parent;
  showLink.value = true;
}

function openAddKey(parent: PlatformAccount): void {
  if (platformStore.mutating) return;
  addKeyError.value = "";
  editKeyAccount.value = null;
  addKeyParent.value = parent;
}

function openEditKey(account: Account): void {
  if (platformStore.mutating) return;
  addKeyError.value = "";
  addKeyParent.value = null;
  editKeyAccount.value = account;
}

function onLinkModalAddKey(): void {
  const parent = linkParent.value;
  showLink.value = false;
  if (parent) openAddKey(parent);
}

function onKeyFormVisible(show: boolean): void {
  if (!show && platformStore.mutating) return;
  if (!show) {
    addKeyParent.value = null;
    editKeyAccount.value = null;
    addKeyError.value = "";
  }
}

/**
 * Two-step lifecycle: create the Custom API account, then associate it. The
 * returned account id is persisted BEFORE any link attempt, so every link
 * failure — CAS conflicts included — transitions to association-only
 * recovery with the same parent and never repeats the create. An ambiguous
 * create outcome (transport failure, 5xx) closes the form and requires an
 * account-list reconciliation before any further attempt: no repeat create
 * can be triggered from the uncertain attempt.
 */
async function onKeyFormSave(payload: PlatformKeyFormPayload): Promise<void> {
  if (editKeyAccount.value) {
    await saveEditedKey(editKeyAccount.value, payload);
    return;
  }
  await createAndLinkKey(payload);
}

async function saveEditedKey(account: Account, payload: PlatformKeyFormPayload): Promise<void> {
  if (!platformStore.beginMutation()) return;
  addKeyError.value = "";
  try {
    const updated = await dashboardApi.updateAccount(account.id, {
      name: payload.name,
      notes: payload.notes,
      ...(payload.key ? { key: payload.key } : {}),
    });
    emit("accountUpdated", updated);
    editKeyAccount.value = null;
    message.success(t("已保存"));
  } catch (error) {
    if (isRevisionConflict(error)) {
      await platformStore.recoverConflict();
      notifyConflict();
      editKeyAccount.value = null;
      return;
    }
    addKeyError.value = dashboardErrorDetail(error);
  } finally {
    platformStore.endMutation();
  }
}

async function createAndLinkKey(payload: PlatformKeyFormPayload): Promise<void> {
  const parent = addKeyParent.value;
  if (!parent || platformStore.mutating) return;
  const hosted = platformHostedEndpoint(parent.baseUrl);
  if (!hosted) {
    addKeyError.value = t("平台地址无效");
    return;
  }
  if (!platformStore.beginMutation()) return;
  addKeyError.value = "";
  try {
    const discovery = await dashboardApi.discoverCustomModels({
      endpoint_url: hosted,
      upstream_protocol: "chat_completions",
      api_key: payload.key,
    });
    if (discovery.models.length === 0) {
      addKeyError.value = t("该 Key 未返回可用模型；确认 Key 与站点地址无误后重试。");
      return;
    }
    const created = await dashboardApi.createAccount({
      name: payload.name,
      key: payload.key,
      notes: payload.notes,
      provider_id: "custom",
      custom_config: {
        endpoint_url: hosted,
        upstream_protocol: "chat_completions",
      },
      model_capabilities: discoveredModelCapabilities(discovery.models),
    });
    platformStore.setPendingLink({ accountId: created.id, parentId: parent.id });
    addKeyParent.value = null;
    try {
      const outcome = await platformStore.link(
        created.id,
        parent.id,
        { id: null, platform: null },
      );
      if (outcome === "conflict") {
        notifyConflict();
        if (platformStore.pendingLink) message.warning(t("Key 已创建，关联尚未完成。"));
        return;
      }
      platformStore.clearPendingLink();
      message.success(overlayImportMessage(created, discovery.truncated));
      emit("changed");
      try {
        await platformStore.commitRefresh(parent.id, created.id);
      } catch {
        // Observation is optional; the Key is already routable.
      }
    } catch {
      message.warning(t("Key 已创建，关联尚未完成。"));
      emit("changed");
    }
  } catch (createError) {
    if (isRevisionConflict(createError)) {
      await platformStore.recoverConflict();
      notifyConflict();
      addKeyParent.value = null;
      return;
    }
    if (createError instanceof DashboardRequestError
      && createError.status >= 400
      && createError.status < 500) {
      addKeyError.value = dashboardErrorDetail(createError);
      return;
    }
    addKeyParent.value = null;
    dialog.warning({
      title: t("创建结果未知"),
      content: t("账号可能已创建；重复提交可能产生重复 Key。重新加载账号列表确认后再继续。"),
      positiveText: t("重新加载"),
      closable: false,
      maskClosable: false,
      onPositiveClick: () => {
        emit("changed");
      },
    });
  } finally {
    platformStore.endMutation();
  }
}

async function fetchModels(account: Account): Promise<void> {
  if (platformStore.mutating) return;
  const hosted = account.custom_config?.endpoint_url
    ? platformHostedEndpoint(account.custom_config.endpoint_url) ?? account.custom_config.endpoint_url
    : "";
  if (!hosted) {
    message.error(t("平台地址无效"));
    return;
  }
  if (!platformStore.beginMutation()) return;
  try {
    const discovery = await dashboardApi.discoverCustomModels({
      endpoint_url: hosted,
      upstream_protocol: account.custom_config?.upstream_protocol ?? "chat_completions",
      account_id: account.id,
    });
    if (discovery.models.length === 0) {
      message.warning(t("该 Key 未返回可用模型；确认 Key 与站点地址无误后重试。"));
      return;
    }
    const updated = await dashboardApi.updateAccountModelCapabilities(
      account.id,
      discoveredModelCapabilities(discovery.models, account.custom_config?.upstream_protocol ?? "chat_completions"),
    );
    emit("accountUpdated", updated);
    message.success(overlayImportMessage(updated, discovery.truncated));
  } catch (error) {
    if (isRevisionConflict(error)) {
      await platformStore.recoverConflict();
      notifyConflict();
    } else {
      mutationError(error, "操作失败：{error}");
    }
  } finally {
    platformStore.endMutation();
  }
}

function overlayImportMessage(account: Account, truncated: boolean): string {
  const imported = truncated
    ? t("已导入 {count} 个模型（列表被截断）", { count: account.model_capabilities.length })
    : t("已导入 {count} 个模型", { count: account.model_capabilities.length });
  const siblings = siblingKeys(account.id).map((item) => (item.id === account.id ? account : item));
  const overlay = platformModelOverlay(siblings);
  const summary = overlay.keys.find((row) => row.accountId === account.id);
  if (!summary || overlay.keys.length < 2 || summary.shared === 0) {
    return imported;
  }
  return t("{imported}；其中 {shared} 个与其他 Key 相同，按 Key 顺序叠加路由，不合并倍率。", {
    imported,
    shared: summary.shared,
  });
}

function siblingKeys(accountId: string): Account[] {
  const parentId = platformStore.links.find((link) => link.accountId === accountId)?.platformAccountId;
  if (!parentId) return [props.accounts.find((account) => account.id === accountId)].filter(Boolean) as Account[];
  const ids = new Set(
    platformStore.links
      .filter((link) => link.platformAccountId === parentId)
      .map((link) => link.accountId),
  );
  return props.accounts.filter((account) => ids.has(account.id));
}

async function fetchModelsAll(accounts: Account[]): Promise<void> {
  for (const account of accounts) {
    await fetchModels(account);
  }
}

/** Retry ONLY the association of an already-created Key; never re-creates. */
async function retryPendingLink(): Promise<void> {
  try {
    const outcome = await platformStore.retryPendingLink();
    if (outcome === "conflict") notifyConflict();
    else if (outcome === "ok") {
      message.success(t("已关联"));
      emit("changed");
    }
  } catch (error) {
    mutationError(error, "操作失败：{error}");
  }
}

async function onLinkSubmit(
  selection: { accountId: string; group: { id: string | null; platform: string | null } },
): Promise<void> {
  const parent = linkParent.value;
  if (!parent || !platformStore.beginMutation()) return;
  try {
    const outcome = await platformStore.link(
      selection.accountId,
      parent.id,
      selection.group,
    );
    if (outcome === "conflict") {
      showLink.value = false;
      notifyConflict();
    } else {
      showLink.value = false;
      message.success(t("已关联"));
      // Linking rewrites the Key's endpoint to the parent-owned inference URL.
      emit("changed");
    }
  } catch (error) {
    mutationError(error, "操作失败：{error}");
  } finally {
    platformStore.endMutation();
  }
}

function confirmUnlink(account: Account, link: PlatformLink): void {
  dialog.warning({
    title: t("取消关联"),
    content: t("确定取消 Key {name} 与该平台账号的关联吗？关联期间写入的平台 Endpoint 会保留为普通 Custom Endpoint。", { name: account.name }),
    positiveText: t("取消关联"),
    negativeText: t("取消"),
    onPositiveClick: () => unlink(link.accountId),
  });
}

async function unlink(accountId: string): Promise<void> {
  try {
    const outcome = await platformStore.unlink(accountId);
    if (outcome === "conflict") notifyConflict();
    else if (outcome === "ok") {
      message.success(t("已取消关联"));
      emit("changed");
    }
  } catch (error) {
    mutationError(error, "操作失败：{error}");
  }
}

onMounted(reload);

defineExpose({
  reload,
  openCreate,
  createPlatform,
  openAddKey,
  importKeys,
  openEditKey,
  fetchModels,
  fetchModelsAll,
  refreshParent,
  refreshChild,
  confirmDelete,
  openEdit,
  openLink,
  retryPendingLink,
  confirmUnlink,
});
</script>
