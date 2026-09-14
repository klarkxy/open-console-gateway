import { t } from "../i18n/index.ts";

const NETWORK_ERROR_PATTERN = /failed to fetch|network(?:error| request failed)|load failed/i;

export function userFacingError(error: unknown, networkFallback: string): string {
  if (error instanceof TypeError && NETWORK_ERROR_PATTERN.test(error.message)) {
    return networkFallback;
  }
  return error instanceof Error ? error.message : String(error);
}

/** Error text for dashboard API failures, with the shared network fallback. */
export function dashboardErrorDetail(error: unknown): string {
  const detail = userFacingError(error, t("无法连接到本地服务，请确认程序正在运行后重试"));
  switch (detail) {
    case "migration password is incorrect or the backup file is damaged":
      return t("迁移包密码错误或文件损坏，请检查密码或重新选择备份文件。");
    case "CPA Management Key is required":
      return t("请填写 CPA Management Key，然后重新测试连接。");
    case "CPA Inference Key is required":
      return t("请填写 CPA Inference Key，然后重新测试连接。");
    case "CPA managed runtime is not installed":
      return t("CPA 尚未安装，请先在概览中安装。");
    case "no desktop Chromium launcher or remote browser worker is configured":
      return t("当前环境无法打开注册浏览器，请使用桌面应用注册，或手动填写 API Key。");
    default:
      return detail;
  }
}
