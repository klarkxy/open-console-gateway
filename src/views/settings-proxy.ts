import type { ProxyMode } from "../api/dashboard";

export function proxyModelKey(model: string): string {
  return model.trim().toLowerCase();
}

export function normalizeProxyUrl(mode: ProxyMode, value: string): string {
  const trimmed = value.trim();
  const urlRequired = mode === "manual" || mode === "list";
  if (!trimmed) {
    if (urlRequired) {
      throw new Error(
        mode === "list" ? "名单模式需要填写代理地址" : "手动代理模式需要填写代理地址",
      );
    }
    return "";
  }

  try {
    return canonicalizeProxyUrl(trimmed);
  } catch (error) {
    if (urlRequired) throw error;
    return trimmed;
  }
}

function canonicalizeProxyUrl(value: string): string {
  let parsed: URL;
  try {
    parsed = new URL(value);
  } catch {
    throw new Error("代理地址格式无效");
  }
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
    throw new Error("代理地址必须是 http:// 或 https:// URL");
  }
  if (!parsed.hostname) throw new Error("代理地址必须包含主机");
  if (parsed.username || parsed.password) throw new Error("代理地址不能包含用户名或密码");
  if ((parsed.pathname && parsed.pathname !== "/") || parsed.search || parsed.hash) {
    throw new Error("代理地址不能包含路径、查询参数或片段");
  }
  return parsed.origin;
}

/**
 * Validates the list-mode model selection against the registry returned by the
 * settings API. Only list mode enforces the list; other modes keep the stored
 * value untouched. Returns the cleaned (trimmed, deduped) list.
 *
 * The unknown-id message embeds the offending ids, so it is NOT an i18n key;
 * Settings.vue filters stored ids against the registry before calling this and
 * therefore only ever surfaces the two static-key errors through t().
 */
export function validateProxyList(
  mode: ProxyMode,
  models: string[],
  supportedIds: string[],
): string[] {
  const cleaned = models.map((model) => model.trim()).filter((model) => model.length > 0);
  if (mode !== "list") return cleaned;
  if (cleaned.length === 0) {
    throw new Error("名单模式至少勾选一个模型");
  }
  const supported = new Set(supportedIds.map(proxyModelKey));
  const unknown = cleaned.filter((model) => !supported.has(proxyModelKey(model)));
  if (unknown.length > 0) {
    throw new Error("名单包含未知模型");
  }
  const seen = new Set<string>();
  return cleaned.filter((model) => {
    const key = proxyModelKey(model);
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}
