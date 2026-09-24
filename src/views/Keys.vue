<template>
  <section class="keys-page" aria-labelledby="gateway-keys-title">
    <div class="keys-card">
      <div class="keys-head">
        <h2 id="gateway-keys-title" class="sr-only">{{ t("接入 Key") }}</h2>
        <p class="field-caption">
          {{ t("主 Key 只能重置。子 Key 可分给不同设备。") }}
        </p>
      </div>

      <n-alert v-if="loadError" type="error" :title="t('接入 Key 加载失败，请重试')">
        <div class="keys-load-error">
          <span>{{ loadError }}</span>
          <n-button size="small" secondary :loading="loading" @click="loadConnection">{{ t("重试") }}</n-button>
        </div>
      </n-alert>

      <ul class="gateway-key-list">
        <li class="gateway-key-row gateway-key-row--primary">
          <div class="gateway-key-main">
            <span class="gateway-key-name">{{ t("主 Key") }}</span>
            <span class="gateway-key-badge">{{ t("恒为有效") }}</span>
          </div>
          <code class="gateway-key-value">{{ maskConnectionKey(connection.primary_key) }}</code>
          <div class="gateway-key-actions">
            <n-tooltip trigger="hover">
              <template #trigger>
                <n-button
                  circle
                  quaternary
                  size="small"
                  :aria-label="t('复制 Key')"
                  :disabled="mutating || !connection.primary_key"
                  @click="copyPrimaryKey"
                >
                  <template #icon><n-icon :component="keyCopied === 'keys-primary' ? CheckOutlined : CopyOutlined" /></template>
                </n-button>
              </template>
              {{ t("复制 Key") }}
            </n-tooltip>
            <n-popconfirm
              :positive-text="t('生成新 Key')"
              :negative-text="t('取消')"
              @positive-click="rotatePrimaryKey"
            >
              <template #trigger>
                <n-tooltip trigger="hover">
                  <template #trigger>
                    <n-button
                      circle
                      quaternary
                      size="small"
                      :aria-label="t('刷新 Key')"
                      :loading="mutating && keyMutation === 'rotate-primary'"
                      :disabled="mutating || !loaded"
                    >
                      <template #icon><n-icon :component="ReloadOutlined" /></template>
                    </n-button>
                  </template>
                  {{ t("刷新 Key") }}
                </n-tooltip>
              </template>
              {{ t("旧 Key 将立即失效，继续生成新 Key？") }}
            </n-popconfirm>
            <span class="gateway-key-slot" aria-hidden="true" />
            <span class="gateway-key-slot gateway-key-slot--switch" aria-hidden="true" />
            <span class="gateway-key-slot" aria-hidden="true" />
          </div>
        </li>
        <li v-for="entry in connection.sub_keys" :key="entry.id" class="gateway-key-row">
          <div class="gateway-key-main">
            <template v-if="renamingKeyId === entry.id">
              <n-input
                v-model:value="renameDraft"
                size="small"
                :disabled="mutating"
                :input-props="{ 'aria-label': t('Key 名称') }"
                @keydown.enter="commitRename(entry)"
              />
              <n-button size="tiny" secondary :disabled="mutating" @click="commitRename(entry)">{{ t("保存") }}</n-button>
              <n-button size="tiny" quaternary :disabled="mutating" @click="cancelRename">{{ t("取消") }}</n-button>
            </template>
            <template v-else>
              <span class="gateway-key-name">{{ entry.name }}</span>
              <span v-if="!entry.enabled" class="gateway-key-badge muted">{{ t("已停用") }}</span>
              <n-tooltip trigger="hover">
                <template #trigger>
                  <n-button
                    circle
                    quaternary
                    size="tiny"
                    :aria-label="t('点击重命名')"
                    :disabled="mutating"
                    @click="startRename(entry)"
                  >
                    <template #icon><n-icon :component="EditOutlined" /></template>
                  </n-button>
                </template>
                {{ t("点击重命名") }}
              </n-tooltip>
            </template>
          </div>
          <code class="gateway-key-value">{{ maskConnectionKey(entry.value) }}</code>
          <div class="gateway-key-actions">
            <n-tooltip trigger="hover">
              <template #trigger>
                <n-button
                  circle
                  quaternary
                  size="small"
                  :aria-label="t('复制 Key')"
                  :disabled="mutating || !entry.value"
                  @click="copyEntryKey(entry)"
                >
                  <template #icon><n-icon :component="keyCopied === `keys-${entry.id}` ? CheckOutlined : CopyOutlined" /></template>
                </n-button>
              </template>
              {{ t("复制 Key") }}
            </n-tooltip>
            <n-popconfirm
              :positive-text="t('生成新 Key')"
              :negative-text="t('取消')"
              @positive-click="regenerateEntryKey(entry)"
            >
              <template #trigger>
                <n-tooltip trigger="hover">
                  <template #trigger>
                    <n-button
                      circle
                      quaternary
                      size="small"
                      :aria-label="t('刷新 Key')"
                      :loading="mutating && keyMutation === `regenerate:${entry.id}`"
                      :disabled="mutating"
                    >
                      <template #icon><n-icon :component="ReloadOutlined" /></template>
                    </n-button>
                  </template>
                  {{ t("刷新 Key") }}
                </n-tooltip>
              </template>
              {{ t("仅当前 Key 的旧值立即失效，其他 Key 不受影响。确定生成新值？") }}
            </n-popconfirm>
            <span class="gateway-key-split" aria-hidden="true" />
            <n-switch
              size="small"
              :value="entry.enabled"
              :loading="mutating && keyMutation === `toggle:${entry.id}`"
              :disabled="mutating"
              :aria-label="t('启用或停用 Key')"
              @update:value="(value: boolean) => toggleKey(entry, value)"
            />
            <n-popconfirm
              :positive-text="t('删除')"
              :negative-text="t('取消')"
              @positive-click="deleteEntryKey(entry)"
            >
              <template #trigger>
                <n-tooltip trigger="hover">
                  <template #trigger>
                    <n-button
                      circle
                      quaternary
                      size="small"
                      type="error"
                      :aria-label="t('删除 Key')"
                      :loading="mutating && keyMutation === `delete:${entry.id}`"
                      :disabled="mutating"
                    >
                      <template #icon><n-icon :component="DeleteOutlined" /></template>
                    </n-button>
                  </template>
                  {{ t("删除 Key") }}
                </n-tooltip>
              </template>
              {{ t("删除后该 Key 立即失效且不可恢复；历史用量仍按名称归因。确定删除？") }}
            </n-popconfirm>
          </div>
        </li>
      </ul>

      <div class="key-create-row">
        <n-input
          v-model:value="newKeyName"
          class="key-create-input"
          :disabled="!loaded || mutating"
          :placeholder="t('新 Key 名称，例如 Laptop')"
          :input-props="{ 'aria-label': t('新 Key 名称') }"
          @keydown.enter="createKey"
        />
        <n-button
          class="key-create-submit"
          secondary
          type="primary"
          size="small"
          :aria-label="t('新建 Key')"
          :loading="mutating && keyMutation === 'create'"
          :disabled="!loaded || mutating || !newKeyName.trim()"
          @click="createKey"
        >{{ t("新建") }}</n-button>
      </div>
    </div>
  </section>
</template>

<script setup lang="ts">
import { computed, onActivated, onMounted, onUnmounted, ref } from "vue";
import {
  NAlert,
  NButton,
  NIcon,
  NInput,
  NPopconfirm,
  NSwitch,
  NTooltip,
  useMessage,
} from "naive-ui";
import {
  CheckOutlined,
  CopyOutlined,
  DeleteOutlined,
  EditOutlined,
  ReloadOutlined,
} from "@vicons/antd";
import { DashboardRequestError } from "../api/dashboard";
import type { ConnectionInfo, ConnectionSubKey } from "../api/dashboard";
import { useConnectionStore } from "../stores/connection.ts";
import { t } from "../i18n/index.ts";
import { useClipboard } from "../utils/format.ts";
import { maskConnectionKey } from "./dashboard-connection";

const message = useMessage();
const connectionStore = useConnectionStore();
const { copiedTarget: keyCopied, copy, cleanup } = useClipboard();
const loaded = ref(false);
const loading = ref(false);
const loadError = ref("");
const mutating = ref(false);
const newKeyName = ref("");
const renamingKeyId = ref("");
const renameDraft = ref("");
const keyMutation = ref("");
let loadGeneration = 0;

const EMPTY_CONNECTION: ConnectionInfo = {
  gateway_port: 9042,
  client_root_url: "",
  primary_key: "",
  sub_keys: [],
  revision: 0,
};
const connection = computed(() => connectionStore.info ?? EMPTY_CONNECTION);

function applyConnection(next: ConnectionInfo): void {
  if (renamingKeyId.value && !next.sub_keys.some((entry) => entry.id === renamingKeyId.value)) {
    cancelRename();
  }
  loaded.value = true;
  loadError.value = "";
}

async function loadConnection(): Promise<boolean> {
  const generation = ++loadGeneration;
  loading.value = true;
  loadError.value = "";
  try {
    const next = await connectionStore.load();
    if (generation !== loadGeneration) return false;
    applyConnection(next);
    return true;
  } catch (error) {
    if (generation !== loadGeneration) return false;
    loadError.value = error instanceof Error ? error.message : String(error);
    message.error(t("加载接入 Key 失败：{error}", { error: loadError.value }));
    return false;
  } finally {
    if (generation === loadGeneration) loading.value = false;
  }
}

function isConflict(error: unknown): boolean {
  return error instanceof DashboardRequestError && error.status === 409;
}

async function runKeyMutation(
  mutation: string,
  action: () => Promise<unknown>,
  successText: () => string,
): Promise<boolean> {
  if (!loaded.value || mutating.value) return false;
  const generation = ++loadGeneration;
  keyMutation.value = mutation;
  mutating.value = true;
  let mutationError: unknown = null;
  try {
    try {
      await action();
    } catch (error) {
      mutationError = error;
    }
    try {
      const latest = await connectionStore.load();
      if (generation !== loadGeneration) return false;
      applyConnection(latest);
      if (mutationError === null) {
        message.success(successText());
        return true;
      }
      if (isConflict(mutationError)) {
        message.warning(t("接入 Key 已被其他操作修改，已刷新列表并保留本地修改，请再次保存"));
      } else {
        message.error(t("操作失败：{error}", { error: String(mutationError) }));
      }
      return false;
    } catch (reloadError) {
      if (generation !== loadGeneration) return false;
      loaded.value = false;
      loadError.value = reloadError instanceof Error ? reloadError.message : String(reloadError);
      if (mutationError === null) {
        message.success(successText());
      } else if (isConflict(mutationError)) {
        message.warning(t("接入 Key 已被其他操作修改，已刷新列表并保留本地修改，请再次保存"));
      } else {
        message.error(t("操作失败：{error}", { error: String(mutationError) }));
      }
      message.error(t("加载接入 Key 失败：{error}", { error: loadError.value }));
      return mutationError === null;
    }
  } finally {
    mutating.value = false;
    keyMutation.value = "";
  }
}

async function rotatePrimaryKey(): Promise<void> {
  let nextValue = "";
  const ok = await runKeyMutation(
    "rotate-primary",
    async () => {
      nextValue = await connectionStore.regeneratePrimaryKey();
    },
    () => t("Key 已刷新"),
  );
  if (ok && nextValue) {
    try {
      await copy(`keys-rotated-${Date.now()}`, nextValue, "Key");
      message.success(t("新 Key 值已复制到剪贴板"));
    } catch {
      message.warning(t("自动复制失败，请在列表中手动复制新 Key"));
    }
  }
}

async function copyPrimaryKey(): Promise<void> {
  if (!connection.value.primary_key) return;
  try {
    await copy("keys-primary", connection.value.primary_key, "Key");
    message.success(t("Key 已复制"));
  } catch (error) {
    message.error(error instanceof Error ? error.message : t("复制失败"));
  }
}

async function createKey(): Promise<void> {
  const name = newKeyName.value.trim();
  if (!name || mutating.value || !loaded.value) return;
  let createdValue = "";
  const ok = await runKeyMutation(
    "create",
    async () => {
      createdValue = (await connectionStore.createKey(name)).value;
    },
    () => t("Key 已创建"),
  );
  if (ok && createdValue) {
    newKeyName.value = "";
    try {
      await copy(`keys-created-${Date.now()}`, createdValue, "Key");
      message.success(t("新 Key 值已复制到剪贴板"));
    } catch {
      message.warning(t("自动复制失败，请在列表中手动复制新 Key"));
    }
  }
}

function startRename(entry: ConnectionSubKey): void {
  renamingKeyId.value = entry.id;
  renameDraft.value = entry.name;
}

function cancelRename(): void {
  renamingKeyId.value = "";
  renameDraft.value = "";
}

async function commitRename(entry: ConnectionSubKey): Promise<void> {
  const name = renameDraft.value.trim();
  if (!name || name === entry.name) {
    cancelRename();
    return;
  }
  await runKeyMutation(
    `rename:${entry.id}`,
    () => connectionStore.updateKey(entry.id, { name }),
    () => t("Key 名称已保存"),
  );
  cancelRename();
}

async function toggleKey(entry: ConnectionSubKey, enabled: boolean): Promise<void> {
  await runKeyMutation(
    `toggle:${entry.id}`,
    () => connectionStore.updateKey(entry.id, { enabled }),
    () => (enabled ? t("Key 已启用") : t("Key 已停用")),
  );
}

async function copyEntryKey(entry: ConnectionSubKey): Promise<void> {
  if (!entry.value) return;
  try {
    await copy(`keys-${entry.id}`, entry.value, "Key");
    message.success(t("Key 已复制"));
  } catch (error) {
    message.error(error instanceof Error ? error.message : t("复制失败"));
  }
}

async function regenerateEntryKey(entry: ConnectionSubKey): Promise<void> {
  let nextValue = "";
  const ok = await runKeyMutation(
    `regenerate:${entry.id}`,
    async () => {
      nextValue = (await connectionStore.regenerateKey(entry.id)).value;
    },
    () => t("Key 已重新生成"),
  );
  if (ok && nextValue) {
    try {
      await copy(`keys-regenerated-${Date.now()}`, nextValue, "Key");
      message.success(t("新 Key 值已复制到剪贴板"));
    } catch {
      message.warning(t("自动复制失败，请在列表中手动复制新 Key"));
    }
  }
}

async function deleteEntryKey(entry: ConnectionSubKey): Promise<void> {
  await runKeyMutation(
    `delete:${entry.id}`,
    () => connectionStore.deleteKey(entry.id),
    () => t("Key 已删除"),
  );
}

onMounted(() => {
  void loadConnection();
});
onActivated(() => {
  if (!loading.value) void loadConnection();
});
onUnmounted(cleanup);
</script>

<style scoped>
.keys-page {
  max-width: 1080px;
  margin: 0 auto;
}
.keys-card {
  padding: 22px;
  border: 1px solid var(--ocg-border);
  border-radius: var(--ocg-radius-lg);
  background: var(--ocg-surface);
  box-shadow: var(--ocg-shadow-sm);
}
.keys-head h2 {
  margin: 0;
  color: var(--ocg-ink);
  font: 700 var(--ocg-font-lg)/1.3 "Bahnschrift", "Segoe UI Variable Display", sans-serif;
}
.field-caption {
  margin: 6px 0 0;
  color: var(--ocg-subtle);
  font-size: var(--ocg-font-sm);
  line-height: 1.5;
}
.keys-load-error {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--ocg-space-md);
}
.key-create-row {
  display: grid;
  grid-template-columns: minmax(0, 1fr) auto;
  align-items: center;
  gap: var(--ocg-space-sm);
  max-width: 28em;
  margin-top: var(--ocg-space-lg);
}
.key-create-submit {
  min-width: 0;
  padding-inline: var(--ocg-space-md);
}
.gateway-key-list {
  display: grid;
  gap: var(--ocg-space-sm);
  margin: var(--ocg-space-lg) 0 0;
  padding: 0;
  list-style: none;
}
.gateway-key-row {
  display: grid;
  grid-template-columns: minmax(11em, 16em) minmax(0, 1fr) auto;
  align-items: center;
  gap: var(--ocg-space-md);
  min-height: 48px;
  padding: var(--ocg-space-sm) var(--ocg-space-md);
  border: 1px solid var(--ocg-border);
  border-radius: var(--ocg-radius-sm);
  background: var(--ocg-canvas);
}
.gateway-key-main {
  display: flex;
  min-width: 0;
  align-items: center;
  gap: var(--ocg-space-sm);
}
.gateway-key-main .n-input {
  max-width: 240px;
}
.gateway-key-name {
  overflow: hidden;
  color: var(--ocg-ink);
  font-size: var(--ocg-font-md);
  font-weight: 600;
  line-height: 1.4;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.gateway-key-badge {
  flex: none;
  padding: 1px var(--ocg-space-sm);
  border: 1px solid var(--ocg-border);
  border-radius: 999px;
  color: var(--ocg-subtle);
  font-size: var(--ocg-font-xs);
}
.gateway-key-badge.muted {
  opacity: 0.8;
}
.gateway-key-value {
  overflow: hidden;
  color: var(--ocg-subtle);
  font-family: "Cascadia Mono", Consolas, monospace;
  font-size: var(--ocg-font-sm);
  text-overflow: ellipsis;
  white-space: nowrap;
}
.gateway-key-actions {
  display: grid;
  grid-template-columns: 32px 32px 12px 40px 32px;
  align-items: center;
  justify-items: center;
}
.gateway-key-split,
.gateway-key-slot {
  display: block;
  width: 100%;
  height: 1px;
}
.gateway-key-slot--switch {
  width: 40px;
}
.gateway-key-row--primary .gateway-key-value {
  color: var(--ocg-muted);
}

@media (max-width: 640px) {
  .gateway-key-row {
    grid-template-columns: minmax(0, 1fr);
    align-items: start;
  }
  .gateway-key-actions {
    justify-self: end;
  }
  .key-create-row {
    max-width: none;
  }
}
</style>
