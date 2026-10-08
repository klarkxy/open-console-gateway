import type { CopilotTarget, CopilotApplicationView } from "../api/copilot-application.ts";
import type { MessageKey } from "../i18n/index.ts";
export const COPILOT_STATUS_KEYS = {
  unsupported_runtime: "此主机支持手动安装", not_detected: "未检测到 VS Code", ready: "可以安装",
  installed_pending: "等待 VS Code 激活", connected: "已连接 OCG", disconnected: "已断开 OCG",
  connection_error: "连接需要处理", conflict: "安装归属冲突",
} as const satisfies Record<CopilotApplicationView["status"], MessageKey>;
export function copilotTargetKey(target: CopilotTarget): string {
  return JSON.stringify([target.installation ?? null, target.profile ?? null, target.userDataDir ?? null, target.extensionsDir ?? null]);
}
export function copilotActionReady(view: CopilotApplicationView | null, action: "install" | "disconnect" | "uninstall", targetCurrent: boolean, busy: boolean): boolean {
  if (!view?.fingerprint || !targetCurrent || busy || view.status === "conflict") return false;
  if (action === "install") return view.installSupported;
  return view.uninstallSupported && (action !== "disconnect" || view.status !== "disconnected");
}
