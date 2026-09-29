/**
 * Shared Intl formatter cache.
 *
 * Constructing `Intl.NumberFormat` / `Intl.DateTimeFormat` is one to two
 * orders of magnitude slower than `format()` (measured ~26.7us vs ~0.51us on
 * this codebase's Node runtime), and the formatting helpers in `src/domain`
 * plus the per-row render helpers in `src/components` run once per table cell
 * or list card. Instances are therefore cached per `(locale, options)` pair,
 * following the same pattern as `utils/format.ts`.
 *
 * The key serializes the locale together with the options, so two call sites
 * that ask for different options never share an instance, and `undefined`
 * (runtime default locale) stays distinct from an explicit locale tag.
 * Options objects are literals at every call site, so serialization is stable
 * and cheap.
 *
 * Invalid options (e.g. a non-ISO currency code, or an empty locale tag) still
 * throw on every call: only successfully constructed formatters are cached,
 * which preserves the call-site try/catch fallbacks that depend on that throw.
 */

const numberFormatters = new Map<string, Intl.NumberFormat>();
const dateTimeFormatters = new Map<string, Intl.DateTimeFormat>();

function cacheKey(
  locale: string | undefined,
  options: Intl.NumberFormatOptions | Intl.DateTimeFormatOptions,
): string {
  return JSON.stringify([locale, options]);
}

/** Locale-aware number formatter, cached per (locale, options). */
export function numberFormatter(locale: string | undefined, options: Intl.NumberFormatOptions): Intl.NumberFormat {
  const key = cacheKey(locale, options);
  let formatter = numberFormatters.get(key);
  if (!formatter) {
    formatter = new Intl.NumberFormat(locale, options);
    numberFormatters.set(key, formatter);
  }
  return formatter;
}

/** Locale-aware date/time formatter, cached per (locale, options). */
export function dateTimeFormatter(locale: string | undefined, options: Intl.DateTimeFormatOptions): Intl.DateTimeFormat {
  const key = cacheKey(locale, options);
  let formatter = dateTimeFormatters.get(key);
  if (!formatter) {
    formatter = new Intl.DateTimeFormat(locale, options);
    dateTimeFormatters.set(key, formatter);
  }
  return formatter;
}
