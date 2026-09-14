import type { AppConfig } from "../api/dashboard";

const EDITABLE_SETTING_KEYS = [
  "gateway_port",
  "proxy_mode",
  "proxy_url",
  "proxy_list_direction",
  "proxy_list_models",
  "client_root_url",
  "auto_start",
  "show_dock_icon",
  "connect_timeout_secs",
  "non_stream_timeout_secs",
  "stream_idle_timeout_secs",
  "routing_mode",
  "conversation_sticky",
] as const satisfies readonly (keyof AppConfig)[];

/**
 * Reference equality misreads array fields: a form clone of the saved value
 * has the same content but a different identity. Compare array content so an
 * untouched clone adopts the latest server array while a genuinely edited
 * array still counts as a local edit.
 */
function settingsValueEqual(a: unknown, b: unknown): boolean {
  if (Array.isArray(a) && Array.isArray(b)) {
    return a.length === b.length && a.every((value, index) => value === b[index]);
  }
  return a === b;
}

/**
 * Keep locally edited fields while adopting a newer server snapshot.
 * Revision, sub keys, environment flags, and capability flags always come
 * from the server and are intentionally excluded from the editable key list.
 */
export function mergeUnsavedSettings(
  latest: AppConfig,
  current: AppConfig,
  saved: AppConfig,
): AppConfig {
  const merged = { ...latest };
  for (const key of EDITABLE_SETTING_KEYS) {
    if (!settingsValueEqual(current[key], saved[key])) {
      Object.assign(merged, { [key]: current[key] });
    }
  }
  return merged;
}
