import { t } from "../i18n/index.ts";

export function maskConnectionKey(key: string): string {
  if (!key) return t("未设置");
  if (key.length <= 4) return "••••";
  if (key.length <= 8) return `${key.slice(0, 2)}…${key.slice(-2)}`;
  return `${key.slice(0, 4)}…${key.slice(-4)}`;
}

export interface ConnectionUrls {
  rootUrl: string;
  apiBaseUrl: string;
  chatCompletionsUrl: string;
  responsesUrl: string;
  messagesUrl: string;
  insecureHttp: boolean;
}

export function normalizeClientRootUrl(value: string): string {
  const input = value.trim();
  if (!input) return "";
  if (!/^https?:\/\//i.test(input)) {
    throw new Error(t("请输入完整的 http:// 或 https:// 地址"));
  }

  let url: URL;
  try {
    url = new URL(input);
  } catch {
    throw new Error(t("请输入有效的绝对 URL"));
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") {
    throw new Error(t("仅支持 HTTP 或 HTTPS 地址"));
  }
  if (!url.hostname) throw new Error(t("地址必须包含主机名"));
  if (url.username || url.password) throw new Error(t("地址不能包含用户名或密码"));
  if (input.includes("?") || input.includes("#")) {
    throw new Error(t("地址不能包含查询参数或 #片段"));
  }

  let path = url.pathname.replace(/\/+$/, "");
  const segments = path.split("/").filter(Boolean);
  const v1Index = segments.findIndex((segment) => segment.toLowerCase() === "v1");
  if (v1Index >= 0) {
    if (v1Index + 1 !== segments.length) {
      throw new Error(t("请填写根地址，不要包含 /v1 后的接口路径"));
    }
    path = path.slice(0, path.length - 3).replace(/\/+$/, "");
  }

  return `${url.origin}${path}`;
}

export function resolveConnectionUrls(
  configuredRoot: string,
  origin: string,
  gatewayPort: number,
  dev: boolean,
): ConnectionUrls {
  const fallbackRoot = dev ? `http://127.0.0.1:${gatewayPort}` : origin;
  const rootUrl = normalizeClientRootUrl(configuredRoot) || normalizeClientRootUrl(fallbackRoot);
  const apiBaseUrl = `${rootUrl}/v1`;
  return {
    rootUrl,
    apiBaseUrl,
    chatCompletionsUrl: `${apiBaseUrl}/chat/completions`,
    responsesUrl: `${apiBaseUrl}/responses`,
    messagesUrl: `${apiBaseUrl}/messages`,
    insecureHttp: isInsecureHttp(rootUrl),
  };
}

function isInsecureHttp(rootUrl: string): boolean {
  const url = new URL(rootUrl);
  if (url.protocol !== "http:") return false;
  const hostname = url.hostname.toLowerCase().replace(/^\[|\]$/g, "");
  const loopback = hostname === "localhost"
    || hostname.endsWith(".localhost")
    || hostname === "::1"
    || /^127(?:\.\d{1,3}){3}$/.test(hostname);
  return !loopback;
}
