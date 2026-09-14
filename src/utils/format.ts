import { ref } from "vue";
import { locale, t } from "../i18n/index.ts";

/**
 * Shared formatting helpers used across Dashboard, Keys, Accounts, Logs, and
 * StackedBarChart. Centralising them here avoids duplication and keeps
 * locale-aware formatting consistent.
 *
 * Intl.NumberFormat construction is one to two orders of magnitude slower than
 * format(), and these helpers run per table cell / per list card, so instances
 * are cached per (locale, fraction digits) combination.
 */
const currencyFormatters = new Map<string, Intl.NumberFormat>();
const numberFormatters = new Map<string, Intl.NumberFormat>();
const compactTokenFormatters = new Map<string, Intl.NumberFormat>();
// Token counts always use en-US compact suffixes (K/M/B). Locale-aware compact
// notation would render 万/億 in CJK locales, which the dashboard rejects.
const COMPACT_TOKEN_LOCALE = "en-US";

function currencyFormatter(localeTag: string, fractionDigits: number): Intl.NumberFormat {
  const cacheKey = `${localeTag}:${fractionDigits}`;
  let formatter = currencyFormatters.get(cacheKey);
  if (!formatter) {
    formatter = new Intl.NumberFormat(localeTag, {
      style: "currency",
      currency: "USD",
      currencyDisplay: "narrowSymbol",
      minimumFractionDigits: fractionDigits,
      maximumFractionDigits: fractionDigits,
    });
    currencyFormatters.set(cacheKey, formatter);
  }
  return formatter;
}

function plainFormatter(localeTag: string): Intl.NumberFormat {
  let formatter = numberFormatters.get(localeTag);
  if (!formatter) {
    formatter = new Intl.NumberFormat(localeTag);
    numberFormatters.set(localeTag, formatter);
  }
  return formatter;
}

function compactTokenFormatter(): Intl.NumberFormat {
  let formatter = compactTokenFormatters.get(COMPACT_TOKEN_LOCALE);
  if (!formatter) {
    formatter = new Intl.NumberFormat(COMPACT_TOKEN_LOCALE, {
      notation: "compact",
      maximumFractionDigits: 1,
    });
    compactTokenFormatters.set(COMPACT_TOKEN_LOCALE, formatter);
  }
  return formatter;
}

/** Locale-aware date+time for timestamps; invalid input renders verbatim. */
export function formatDateTime(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return new Intl.DateTimeFormat(locale.value, {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).format(date);
}

/** Format a number as USD currency with adaptive or caller-specified decimal places. */
export function formatCost(value: number, digits?: number): string {
  const fractionDigits = digits ?? (value !== 0 && Math.abs(value) < 0.01 ? 4 : 2);
  return currencyFormatter(locale.value, fractionDigits).format(value);
}

/** Format a number with locale-aware grouping. */
export function formatNumber(value: number): string {
  return plainFormatter(locale.value).format(value);
}

/** Format a token count with a compact K/M/B suffix (e.g. 12.3K), locale-independent. */
export function formatTokens(value: number): string {
  return compactTokenFormatter().format(value);
}

/**
 * Composable-style clipboard helper with visual feedback state.
 *
 * Usage in `<script setup>`:
 * ```ts
 * const { copiedTarget, copy, cleanup } = useClipboard();
 * await copy('key', someValue, 'Key');
 * // …onUnmounted(() => cleanup());
 * ```
 */
export function useClipboard(timeout = 1500) {
  const copiedTarget = ref<string | null>(null);
  let timer: ReturnType<typeof setTimeout> | undefined;

  async function copy(target: string, value: string, label: string) {
    const writeText = navigator.clipboard?.writeText?.bind(navigator.clipboard);
    if (!value) throw new Error(t("没有可复制的内容"));
    if (!writeText) throw new Error(t("当前环境不支持剪贴板"));
    await writeText(value);
    copiedTarget.value = target;
    clearTimeout(timer);
    timer = setTimeout(() => { copiedTarget.value = null; }, timeout);
    return { target, label };
  }

  function cleanup() {
    clearTimeout(timer);
  }

  return { copiedTarget, copy, cleanup };
}
