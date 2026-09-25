<template>
  <main class="browser-session">
    <header class="browser-session__header">
      <div>
        <p class="browser-session__eyebrow">Open Console Gateway</p>
        <h1>{{ t("远程浏览器") }}</h1>
      </div>
      <n-space align="center">
        <n-tag :type="statusType" size="small">{{ statusLabel }}</n-tag>
        <n-button secondary @click="closeBrowserView">{{ t("关闭页面") }}</n-button>
      </n-space>
    </header>

    <n-alert
      v-if="error"
      type="error"
      :title="t('远程浏览器连接失败')"
      class="browser-session__alert"
    >
      <n-space vertical>
        <span>{{ error }}</span>
        <n-button size="small" secondary @click="connect">{{ t("重新连接") }}</n-button>
      </n-space>
    </n-alert>

    <section class="browser-session__workspace">
      <div class="browser-session__screen-shell">
        <div
          ref="screen"
          class="browser-session__screen"
          :aria-label="t('远程 Chromium 画面')"
        />
        <div v-if="connecting" class="browser-session__loading" role="status">
          <n-spin size="large" />
          <span>{{ t("正在连接远程浏览器…") }}</span>
        </div>
      </div>

      <aside class="browser-session__clipboard" aria-labelledby="remote-clipboard-title">
        <div>
          <h2 id="remote-clipboard-title">{{ t("远程剪贴板") }}</h2>
          <p>{{ t("在这里与远程 Chromium 交换文本；Key 只在你主动复制或发送时进入剪贴板。") }}</p>
        </div>
        <n-input
          v-model:value="clipboardText"
          type="textarea"
          :autosize="{ minRows: 5, maxRows: 12 }"
          :placeholder="t('从 OpenCode 复制的 Key 会显示在这里，也可粘贴文本发送到远程浏览器。')"
          :input-props="{ 'aria-label': t('远程剪贴板内容') }"
        />
        <n-space vertical>
          <n-button
            type="primary"
            block
            :disabled="!connected || !clipboardText"
            @click="sendClipboard"
          >{{ t("发送到远程浏览器") }}</n-button>
          <n-button
            block
            secondary
            :disabled="!clipboardText"
            @click="copyClipboard"
          >{{ t("复制到本机剪贴板") }}</n-button>
          <n-button block quaternary @click="clipboardText = ''">{{ t("清空剪贴板区域") }}</n-button>
        </n-space>
        <n-alert type="warning" :show-icon="false">
          {{ t("远程会话空闲 30 分钟后失效，最长保留 4 小时；新开其他账号会使当前画面立即失效。") }}
        </n-alert>
      </aside>
    </section>
  </main>
</template>

<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref } from "vue";
import { NAlert, NButton, NInput, NSpace, NSpin, NTag, useMessage } from "naive-ui";
import RFB from "@novnc/novnc";
import type { RFBClipboardEvent } from "@novnc/novnc";
import { browserSessionWebSocketUrl } from "../api/dashboard";
import { t } from "../i18n/index.ts";

const props = defineProps<{ sessionToken: string }>();
const message = useMessage();
const screen = ref<HTMLElement | null>(null);
const clipboardText = ref("");
const connecting = ref(false);
const connected = ref(false);
const error = ref("");
let rfb: RFB | null = null;

const statusLabel = computed(() => {
  if (connected.value) return t("已连接");
  if (connecting.value) return t("连接中");
  return t("已断开");
});
const statusType = computed<"success" | "warning" | "error">(() => (
  connected.value ? "success" : connecting.value ? "warning" : "error"
));

function disposeRfb(): void {
  const client = rfb;
  if (!client) return;
  rfb = null;
  client.disconnect();
}

async function connect(): Promise<void> {
  disposeRfb();
  error.value = "";
  connected.value = false;
  if (!props.sessionToken) {
    error.value = t("缺少远程浏览器会话令牌，请从账号页重新打开。");
    return;
  }
  connecting.value = true;
  await nextTick();
  if (!screen.value) return;
  try {
    const client = new RFB(screen.value, browserSessionWebSocketUrl(props.sessionToken), { shared: false });
    client.background = "#111318";
    client.clipViewport = false;
    client.focusOnClick = true;
    client.resizeSession = true;
    // Display-side scaling only: the canvas stays at framebuffer size (1:1
    // blits), and the CSS below fits it into the shell. scaleViewport would
    // make the WebView composite a full-size bitmap on every frame.
    client.scaleViewport = false;
    client.viewOnly = false;
    client.addEventListener("connect", () => {
      if (rfb !== client) return;
      connecting.value = false;
      connected.value = true;
      error.value = "";
    });
    client.addEventListener("disconnect", () => {
      if (rfb !== client) return;
      connecting.value = false;
      connected.value = false;
      error.value = t("会话已断开或失效，请回到账号页重新打开浏览器。");
    });
    client.addEventListener("securityfailure", () => {
      if (rfb !== client) return;
      connecting.value = false;
      connected.value = false;
      error.value = t("远程浏览器鉴权失败，请回到账号页重新打开。");
    });
    client.addEventListener("clipboard", ((event: RFBClipboardEvent) => {
      if (rfb !== client) return;
      clipboardText.value = event.detail.text;
      message.success(t("已收到远程剪贴板内容"));
    }) as EventListener);
    rfb = client;
  } catch (cause) {
    connecting.value = false;
    error.value = cause instanceof Error ? cause.message : String(cause);
  }
}

function sendClipboard(): void {
  if (!rfb || !connected.value || !clipboardText.value) return;
  rfb.clipboardPasteFrom(clipboardText.value);
  message.success(t("已发送到远程浏览器"));
}

async function copyClipboard(): Promise<void> {
  try {
    await navigator.clipboard.writeText(clipboardText.value);
    message.success(t("已复制到本机剪贴板"));
  } catch {
    message.error(t("复制失败"));
  }
}

function closeBrowserView(): void {
  window.close();
  if (!window.closed) {
    const url = new URL(window.location.href);
    url.searchParams.set("view", "accounts");
    url.searchParams.delete("session");
    url.hash = "";
    window.location.assign(url);
  }
}

onMounted(() => {
  void connect();
});
onUnmounted(disposeRfb);
</script>

<style scoped>
.browser-session {
  min-height: 100vh;
  padding: 20px;
  background: var(--ocg-canvas);
  color: var(--ocg-ink);
}

.browser-session__header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--ocg-space-lg);
  max-width: 1680px;
  margin: 0 auto var(--ocg-space-lg);
}

.browser-session__header h1,
.browser-session__clipboard h2 {
  margin: 0;
}

.browser-session__header h1 {
  font-size: var(--ocg-font-xl);
}

.browser-session__eyebrow {
  margin: 0 0 var(--ocg-space-xs);
  color: var(--ocg-primary);
  font-size: var(--ocg-font-xs);
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
}

.browser-session__alert,
.browser-session__workspace {
  max-width: 1680px;
  margin-right: auto;
  margin-left: auto;
}

.browser-session__alert {
  margin-bottom: var(--ocg-space-lg);
}

.browser-session__workspace {
  display: grid;
  grid-template-columns: minmax(0, 1fr) 320px;
  gap: var(--ocg-space-lg);
  min-height: calc(100vh - 108px);
}

.browser-session__screen-shell,
.browser-session__clipboard {
  border: 1px solid var(--ocg-divider);
  border-radius: var(--ocg-radius-lg);
  background: var(--ocg-surface);
  box-shadow: var(--ocg-shadow-sm);
}

.browser-session__screen-shell {
  position: relative;
  min-height: 640px;
  overflow: hidden;
  background: #111318;
}

.browser-session__screen {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 100%;
  height: 100%;
  min-height: 640px;
  overflow: hidden;
}

/* noVNC sizes the canvas to the framebuffer; contain-scaling keeps the full
   remote picture visible inside the shell without per-frame compositing. */
.browser-session__screen :deep(canvas) {
  max-width: 100%;
  max-height: 100%;
  object-fit: contain;
}

.browser-session__loading {
  position: absolute;
  inset: 0;
  display: grid;
  place-content: center;
  justify-items: center;
  gap: var(--ocg-space-md);
  color: #f7f7fa;
  background: rgba(17, 19, 24, 0.82);
}

.browser-session__clipboard {
  align-self: start;
  display: grid;
  gap: var(--ocg-space-lg);
  padding: 18px;
}

.browser-session__clipboard h2 {
  font-size: var(--ocg-font-lg);
}

.browser-session__clipboard p {
  margin: 6px 0 0;
  color: var(--ocg-muted);
  font-size: var(--ocg-font-sm);
  line-height: 1.6;
}

@media (max-width: 980px) {
  .browser-session__workspace {
    grid-template-columns: 1fr;
  }

  .browser-session__clipboard {
    width: auto;
  }
}
</style>
