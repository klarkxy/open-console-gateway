<template>
  <n-alert
    v-if="loadError"
    type="error"
    :title="t('加载平台账号失败: {error}', { error: loadError })"
  >
    <n-button size="small" secondary @click="load">{{ t("重试") }}</n-button>
  </n-alert>

  <PlatformAccountFormModal
    :show="showForm"
    :editing="editingPlatform"
    :preset-kind="presetKind"
    :busy="mutating"
    @update:show="showForm = $event"
    @save="onFormSave"
  />
  <PlatformLinkModal
    :show="showLink"
    :parent="linkParent"
    :candidates="linkCandidates"
    :busy="mutating"
    @update:show="showLink = $event"
    @submit="onLinkSubmit"
    @add-key="onLinkModalAddKey"
  />
  <PlatformKeyFormModal
    :show="!!addKeyParent || !!editKeyAccount"
    :parent-name="keyFormParentName"
    :title="editKeyAccount ? t('编辑 Key') : t('添加 Key')"
    :editing="editKeyAccount ? { name: editKeyAccount.name, notes: editKeyAccount.notes } : null"
    :busy="mutating"
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
  platformAccountsApi,
  platformGroupWrite,
  type PlatformAccount,
  type PlatformAccountsView,
  type PlatformKind,
  type PlatformLink,
} from "../api/platform-accounts.ts";
import {
  discoveredModelCapabilities,
  linkedAccountIdSet,
  platformHostedEndpoint,
  platformModelOverlay,
} from "../domain/platform-accounts.ts";
import { isCustomApiAccount } from "../domain/custom-account.ts";
import { t, type MessageKey } from "../i18n/index.ts";
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
  /** A capabilities import returned the updated account; replace it in place. */
  accountUpdated: [account: Account];
  /** Latest link set plus parent names, for the parent-owned-endpoint lock. */
  linksChange: [links: PlatformLink[], parents: PlatformAccount[]];
}>();

const dialog = useDialog();
const message = useMessage();

const view = ref<PlatformAccountsView | null>(null);
const loading = ref(true);
const loadError = ref("");
const mutating = ref(false);
const refreshing = ref<Record<string, boolean>>({});

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
const pendingLink = ref<{ accountId: string; parentId: string } | null>(null);
const editKeyAccount = ref<Account | null>(null);

const keyFormParentName = computed(() => {
  if (addKeyParent.value) return addKeyParent.value.name;
  if (!editKeyAccount.value) return "";
  const link = (view.value?.links ?? []).find((item) => item.accountId === editKeyAccount.value!.id);
  return (view.value?.accounts ?? []).find((parent) => parent.id === link?.platformAccountId)?.name ?? "";
});

const linkCandidates = computed(() => {
  const linked = linkedAccountIdSet(view.value?.links ?? []);
  return props.accounts.filter((account) => isCustomApiAccount(account) && !linked.has(account.id));
});

watch(() => view.value?.links, (links) => {
  emit(
    "linksChange",
    links ?? [],
    view.value?.accounts ?? [],
  );
  // A reloaded view that already contains the pending account's link settles
  // the retry state without another write.
  if (pendingLink.value && (links ?? []).some((link) => link.accountId === pendingLink.value!.accountId)) {
    pendingLink.value = null;
  }
});

function linksFor(parent: PlatformAccount): PlatformLink[] {
  return (view.value?.links ?? []).filter((link) => link.platformAccountId === parent.id);
}

// Overlapping loads resolve out of order; only the latest operation commits
// loading/error presentation. Mirrors the load guard in stores/accounts.ts.
let loadGeneration = 0;

/**
 * Acceptance boundary for every incoming snapshot: within the same backend
 * process generation a delayed older response must not roll the view back;
 * a different generation is an opaque identity with no comparable ordering,
 * so it is adopted as-is. Mirrors the revision sink in stores/controlPlane.ts.
 *
 * An accepted snapshot is a complete fresh list, so it also supersedes any
 * pending load: drop the obsolete loading/error presentation and invalidate
 * older load completions. A rejected stale snapshot carries no fresh state
 * and leaves an in-flight newer load untouched.
 */
function acceptView(next: PlatformAccountsView): void {
  const current = view.value;
  if (
    current !== null
    && next.processGeneration === current.processGeneration
    && next.revision < current.revision
  ) {
    return;
  }
  view.value = next;
  loadGeneration += 1;
  loading.value = false;
  loadError.value = "";
}

async function load(): Promise<void> {
  const generation = ++loadGeneration;
  loading.value = true;
  loadError.value = "";
  try {
    acceptView(await platformAccountsApi.list());
  } catch (error) {
    if (generation === loadGeneration) {
      loadError.value = dashboardErrorDetail(error);
      message.error(t("加载平台账号失败: {error}", { error: loadError.value }));
    }
  } finally {
    if (generation === loadGeneration) loading.value = false;
  }
}

/** Revision-conflict recovery: tokens already refreshed by the CAS layer; reload and ask to retry. */
async function recoverConflict(): Promise<void> {
  try {
    acceptView(await platformAccountsApi.list());
  } catch {
    // The next explicit action retries; keep the conflict warning meaningful.
  }
  message.warning(t("账号设置已被其他操作修改，已重新加载最新状态，请重试"));
  emit("changed");
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

type PlatformPersistOutcome = "saved" | "conflict" | "error";

/**
 * Single owner of the platform create/update write: the edit modal and the
 * Add Account chooser's embedded create form both funnel through here so
 * validation results, CAS conflict recovery, and the card reload match.
 */
async function persistPlatform(
  payload: PlatformAccountFormPayload,
  editing: PlatformAccount | null,
): Promise<PlatformPersistOutcome> {
  if (mutating.value) return "error";
  mutating.value = true;
  try {
    acceptView(editing
      ? await platformAccountsApi.update(editing.id, {
        name: payload.name,
        ...(payload.userCredential !== undefined ? { userCredential: payload.userCredential } : {}),
      })
      : await platformAccountsApi.create({
        kind: payload.kind,
        name: payload.name,
        baseUrl: payload.baseUrl,
        ...(payload.userCredential !== undefined ? { userCredential: payload.userCredential } : {}),
      }));
    message.success(editing ? t("平台账号已更新") : t("平台账号已创建"));
    return "saved";
  } catch (error) {
    if (isRevisionConflict(error)) {
      await recoverConflict();
      return "conflict";
    }
    mutationError(error, "保存失败: {error}");
    return "error";
  } finally {
    mutating.value = false;
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
  return (await persistPlatform(payload, null)) === "saved";
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
  if (mutating.value) return;
  mutating.value = true;
  try {
    await platformAccountsApi.remove(parent.id);
    await load();
    message.success(t("平台账号已删除"));
  } catch (error) {
    if (isRevisionConflict(error)) await recoverConflict();
    else mutationError(error, "删除失败: {error}");
  } finally {
    mutating.value = false;
  }
}

async function refreshParent(parent: PlatformAccount): Promise<void> {
  if (refreshing.value[parent.id]) return;
  refreshing.value[parent.id] = true;
  try {
    acceptView(await platformAccountsApi.refresh(parent.id));
    message.success(t("已刷新"));
  } catch (error) {
    if (isRevisionConflict(error)) await recoverConflict();
    else mutationError(error, "刷新失败: {error}");
  } finally {
    refreshing.value[parent.id] = false;
  }
}

async function refreshChild(parent: PlatformAccount, link: PlatformLink): Promise<void> {
  const key = `${parent.id}:${link.accountId}`;
  if (refreshing.value[key]) return;
  refreshing.value[key] = true;
  try {
    acceptView(await platformAccountsApi.refresh(parent.id, link.accountId));
    message.success(t("已刷新"));
  } catch (error) {
    if (isRevisionConflict(error)) await recoverConflict();
    else mutationError(error, "刷新失败: {error}");
  } finally {
    refreshing.value[key] = false;
  }
}

function openLink(parent: PlatformAccount): void {
  linkParent.value = parent;
  showLink.value = true;
}

function openAddKey(parent: PlatformAccount): void {
  if (mutating.value) return;
  addKeyError.value = "";
  editKeyAccount.value = null;
  addKeyParent.value = parent;
}

function openEditKey(account: Account): void {
  if (mutating.value) return;
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
  if (!show && mutating.value) return;
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
  if (mutating.value) return;
  mutating.value = true;
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
      await recoverConflict();
      editKeyAccount.value = null;
      return;
    }
    addKeyError.value = dashboardErrorDetail(error);
  } finally {
    mutating.value = false;
  }
}

async function createAndLinkKey(payload: PlatformKeyFormPayload): Promise<void> {
  const parent = addKeyParent.value;
  if (!parent || mutating.value) return;
  const hosted = platformHostedEndpoint(parent.baseUrl);
  if (!hosted) {
    addKeyError.value = t("平台地址无效");
    return;
  }
  mutating.value = true;
  addKeyError.value = "";
  try {
    const discovery = await dashboardApi.discoverCustomModels({
      endpoint_url: hosted,
      upstream_protocol: "chat_completions",
      api_key: payload.key,
    });
    if (discovery.models.length === 0) {
      addKeyError.value = t("该 Key 没有返回可用模型，请确认 Key 与站点地址后重试。");
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
    pendingLink.value = { accountId: created.id, parentId: parent.id };
    addKeyParent.value = null;
    try {
      acceptView(await platformAccountsApi.link(
        created.id,
        parent.id,
        platformGroupWrite({ id: null, platform: null }),
      ));
      pendingLink.value = null;
      message.success(overlayImportMessage(created, discovery.truncated));
      emit("changed");
      try {
        acceptView(await platformAccountsApi.refresh(parent.id, created.id));
      } catch {
        // Observation is optional; the Key is already routable.
      }
    } catch (linkError) {
      if (isRevisionConflict(linkError)) {
        await recoverConflict();
        if (pendingLink.value) message.warning(t("Key 已创建，关联尚未完成。"));
        return;
      }
      message.warning(t("Key 已创建，关联尚未完成。"));
      emit("changed");
    }
  } catch (createError) {
    if (isRevisionConflict(createError)) {
      await recoverConflict();
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
      content: t("账号可能已创建；直接重复提交可能产生重复 Key。请重新加载账号列表确认后再继续。"),
      positiveText: t("重新加载"),
      closable: false,
      maskClosable: false,
      onPositiveClick: () => {
        emit("changed");
      },
    });
  } finally {
    mutating.value = false;
  }
}

async function fetchModels(account: Account): Promise<void> {
  if (mutating.value) return;
  const hosted = account.custom_config?.endpoint_url
    ? platformHostedEndpoint(account.custom_config.endpoint_url) ?? account.custom_config.endpoint_url
    : "";
  if (!hosted) {
    message.error(t("平台地址无效"));
    return;
  }
  mutating.value = true;
  try {
    const discovery = await dashboardApi.discoverCustomModels({
      endpoint_url: hosted,
      upstream_protocol: account.custom_config?.upstream_protocol ?? "chat_completions",
      account_id: account.id,
    });
    if (discovery.models.length === 0) {
      message.warning(t("该 Key 没有返回可用模型，请确认 Key 与站点地址后重试。"));
      return;
    }
    const updated = await dashboardApi.updateAccountModelCapabilities(
      account.id,
      discoveredModelCapabilities(discovery.models, account.custom_config?.upstream_protocol ?? "chat_completions"),
    );
    emit("accountUpdated", updated);
    message.success(overlayImportMessage(updated, discovery.truncated));
  } catch (error) {
    if (isRevisionConflict(error)) await recoverConflict();
    else mutationError(error, "操作失败: {error}");
  } finally {
    mutating.value = false;
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
  const parentId = (view.value?.links ?? []).find((link) => link.accountId === accountId)?.platformAccountId;
  if (!parentId) return [props.accounts.find((account) => account.id === accountId)].filter(Boolean) as Account[];
  const ids = new Set(
    (view.value?.links ?? [])
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
  const pending = pendingLink.value;
  if (!pending || mutating.value) return;
  mutating.value = true;
  try {
    acceptView(await platformAccountsApi.link(
      pending.accountId,
      pending.parentId,
      platformGroupWrite({ id: null, platform: null }),
    ));
    pendingLink.value = null;
    message.success(t("已关联"));
    emit("changed");
  } catch (error) {
    if (isRevisionConflict(error)) await recoverConflict();
    else mutationError(error, "操作失败: {error}");
  } finally {
    mutating.value = false;
  }
}

async function onLinkSubmit(
  selection: { accountId: string; group: { id: string | null; platform: string | null } },
): Promise<void> {
  const parent = linkParent.value;
  if (!parent || mutating.value) return;
  mutating.value = true;
  try {
    acceptView(await platformAccountsApi.link(
      selection.accountId,
      parent.id,
      platformGroupWrite(selection.group),
    ));
    showLink.value = false;
    message.success(t("已关联"));
    // Linking rewrites the Key's endpoint to the parent-owned inference URL.
    emit("changed");
  } catch (error) {
    if (isRevisionConflict(error)) {
      showLink.value = false;
      await recoverConflict();
    } else {
      mutationError(error, "操作失败: {error}");
    }
  } finally {
    mutating.value = false;
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
  if (mutating.value) return;
  mutating.value = true;
  try {
    acceptView(await platformAccountsApi.unlink(accountId));
    message.success(t("已取消关联"));
    emit("changed");
  } catch (error) {
    if (isRevisionConflict(error)) await recoverConflict();
    else mutationError(error, "操作失败: {error}");
  } finally {
    mutating.value = false;
  }
}

onMounted(load);

defineExpose({
  reload: load,
  openCreate,
  createPlatform,
  mutating,
  view,
  pendingLink,
  refreshing,
  openAddKey,
  openEditKey,
  fetchModels,
  fetchModelsAll,
  linksFor,
  refreshParent,
  refreshChild,
  confirmDelete,
  openEdit,
  openLink,
  retryPendingLink,
  confirmUnlink,
});
</script>
