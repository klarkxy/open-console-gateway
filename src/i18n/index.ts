import { computed, ref, shallowRef, watch } from "vue";
import type { NDateLocale, NLocale } from "naive-ui";
// Direct module paths (not the locales barrel) keep the eagerly bundled set to
// the two default locales; every other naive-ui locale loads on demand below.
import naiveEnUS from "naive-ui/es/locales/common/enUS.mjs";
import dateEnUS from "naive-ui/es/locales/date/enUS.mjs";
import naiveZhCN from "naive-ui/es/locales/common/zhCN.mjs";
import dateZhCN from "naive-ui/es/locales/date/zhCN.mjs";
import { enUSMessages, type MessageKey, type Messages } from "./messages/en-US.ts";

export type { MessageKey, Messages } from "./messages/en-US.ts";

export const LOCALE_STORAGE_KEY = "ocg-manager.locale";
export const DEFAULT_LOCALE = "zh-CN";
export const LOCALE_OPTIONS = [
  { value: "zh-CN", label: "简体中文" },
  { value: "zh-TW", label: "繁體中文" },
  { value: "en-US", label: "English" },
  { value: "ja-JP", label: "日本語" },
  { value: "ko-KR", label: "한국어" },
  { value: "es-ES", label: "Español" },
  { value: "fr-FR", label: "Français" },
  { value: "de-DE", label: "Deutsch" },
  { value: "pt-BR", label: "Português (Brasil)" },
  { value: "ru-RU", label: "Русский" },
] as const;

export type Locale = (typeof LOCALE_OPTIONS)[number]["value"];
export type TranslationParams = Record<string, string | number>;

const localeValues = new Set<string>(LOCALE_OPTIONS.map(({ value }) => value));
const zhCNMessages = Object.fromEntries(
  Object.keys(enUSMessages).map((key) => [key, key]),
) as Messages;

type NaiveLocalePair = { locale: NLocale; dateLocale: NDateLocale };

// zh-CN derives from the en-US key set, so only these two catalogs are eager.
const catalogs = new Map<Locale, Messages>([
  ["zh-CN", zhCNMessages],
  ["en-US", enUSMessages],
]);
const naivePairs = shallowRef<Map<Locale, NaiveLocalePair>>(new Map([
  ["zh-CN", { locale: naiveZhCN, dateLocale: dateZhCN }],
  ["en-US", { locale: naiveEnUS, dateLocale: dateEnUS }],
]));

// Lazy catalogs merge over the en-US base so a partial translation still
// renders every key, matching the previous eager `{ ...enUSMessages, ... }`.
const lazyLoaders: Partial<Record<Locale, () => Promise<Messages>>> = {
  "zh-TW": async () => ({ ...enUSMessages, ...(await import("./messages/zh-TW.ts")).zhTWMessages }),
  "ja-JP": async () => ({ ...enUSMessages, ...(await import("./messages/ja-JP.ts")).jaJPMessages }),
  "ko-KR": async () => ({ ...enUSMessages, ...(await import("./messages/ko-KR.ts")).koKRMessages }),
  "es-ES": async () => ({ ...enUSMessages, ...(await import("./messages/es-ES.ts")).esESMessages }),
  "fr-FR": async () => ({ ...enUSMessages, ...(await import("./messages/fr-FR.ts")).frFRMessages }),
  "de-DE": async () => ({ ...enUSMessages, ...(await import("./messages/de-DE.ts")).deDEMessages }),
  "pt-BR": async () => ({ ...enUSMessages, ...(await import("./messages/pt-BR.ts")).ptBRMessages }),
  "ru-RU": async () => ({ ...enUSMessages, ...(await import("./messages/ru-RU.ts")).ruRUMessages }),
};

// naive-ui has no es-ES pack; es-AR covers the Spanish variants it ships.
const lazyNaivePairs: Partial<Record<Locale, () => Promise<NaiveLocalePair>>> = {
  "zh-TW": async () => ({
    locale: (await import("naive-ui/es/locales/common/zhTW.mjs")).default,
    dateLocale: (await import("naive-ui/es/locales/date/zhTW.mjs")).default,
  }),
  "ja-JP": async () => ({
    locale: (await import("naive-ui/es/locales/common/jaJP.mjs")).default,
    dateLocale: (await import("naive-ui/es/locales/date/jaJP.mjs")).default,
  }),
  "ko-KR": async () => ({
    locale: (await import("naive-ui/es/locales/common/koKR.mjs")).default,
    dateLocale: (await import("naive-ui/es/locales/date/koKR.mjs")).default,
  }),
  "es-ES": async () => ({
    locale: (await import("naive-ui/es/locales/common/esAR.mjs")).default,
    dateLocale: (await import("naive-ui/es/locales/date/esAR.mjs")).default,
  }),
  "fr-FR": async () => ({
    locale: (await import("naive-ui/es/locales/common/frFR.mjs")).default,
    dateLocale: (await import("naive-ui/es/locales/date/frFR.mjs")).default,
  }),
  "de-DE": async () => ({
    locale: (await import("naive-ui/es/locales/common/deDE.mjs")).default,
    dateLocale: (await import("naive-ui/es/locales/date/deDE.mjs")).default,
  }),
  "pt-BR": async () => ({
    locale: (await import("naive-ui/es/locales/common/ptBR.mjs")).default,
    dateLocale: (await import("naive-ui/es/locales/date/ptBR.mjs")).default,
  }),
  "ru-RU": async () => ({
    locale: (await import("naive-ui/es/locales/common/ruRU.mjs")).default,
    dateLocale: (await import("naive-ui/es/locales/date/ruRU.mjs")).default,
  }),
};

const inflightLoads = new Map<Locale, Promise<void>>();
let localeRequest = 0;

async function ensureLocaleLoaded(value: Locale): Promise<void> {
  if (catalogs.has(value)) return;
  let load = inflightLoads.get(value);
  if (!load) {
    load = Promise.all([
      (lazyLoaders[value] ?? (async () => enUSMessages))(),
      lazyNaivePairs[value]?.() ?? Promise.resolve(null),
    ])
      .then(([catalog, pair]) => {
        catalogs.set(value, catalog);
        if (pair) {
          const next = new Map(naivePairs.value);
          next.set(value, pair);
          naivePairs.value = next;
        }
      })
      .finally(() => {
        inflightLoads.delete(value);
      });
    inflightLoads.set(value, load);
  }
  return load;
}

export function isLocale(value: string | null | undefined): value is Locale {
  return typeof value === "string" && localeValues.has(value);
}

export function matchLocale(value: string | null | undefined): Locale | null {
  if (!value) return null;
  const normalized = value.replaceAll("_", "-").toLowerCase();
  const exact = LOCALE_OPTIONS.find(({ value: option }) => option.toLowerCase() === normalized);
  if (exact) return exact.value;
  const [language] = normalized.split("-");
  if (language === "zh") {
    return /(?:^|-)hant(?:-|$)|-(?:tw|hk|mo)(?:-|$)/.test(normalized) ? "zh-TW" : "zh-CN";
  }
  return LOCALE_OPTIONS.find(({ value: option }) => option.toLowerCase().startsWith(`${language}-`))?.value ?? null;
}

export function resolveLocale(
  stored: string | null | undefined,
  preferred: readonly string[] = [],
): Locale {
  return matchLocale(stored)
    ?? preferred.map(matchLocale).find((value): value is Locale => value !== null)
    ?? DEFAULT_LOCALE;
}

export function getLocaleStorage(): Storage | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

export function readLocale(
  storage: Pick<Storage, "getItem"> | null,
  preferred: readonly string[] = [],
): Locale {
  try {
    return resolveLocale(storage?.getItem(LOCALE_STORAGE_KEY), preferred);
  } catch {
    return resolveLocale(null, preferred);
  }
}

export function writeLocale(storage: Pick<Storage, "setItem"> | null, value: Locale): void {
  try {
    storage?.setItem(LOCALE_STORAGE_KEY, value);
  } catch {
    // A private or locked-down browser may reject persistence; the in-memory locale still works.
  }
}

function browserLocales(): readonly string[] {
  if (typeof window === "undefined" || typeof navigator === "undefined") return [];
  return navigator.languages?.length ? navigator.languages : navigator.language ? [navigator.language] : [];
}

const localeStorage = getLocaleStorage();
export const locale = ref<Locale>(readLocale(localeStorage, browserLocales()));
export const localeLabel = computed(() => (
  LOCALE_OPTIONS.find(({ value }) => value === locale.value)?.label ?? locale.value
));
// Fall back to the en-US packs until a lazy locale finishes loading.
export const naiveLocale = computed(
  () => naivePairs.value.get(locale.value)?.locale ?? naiveEnUS,
);
export const naiveDateLocale = computed(
  () => naivePairs.value.get(locale.value)?.dateLocale ?? dateEnUS,
);

// Swapped whenever the active catalog changes so every `t()` call site that
// reads it inside a render or computed re-evaluates.
const activeCatalog = shallowRef<Messages>(
  catalogs.get(locale.value) ?? zhCNMessages,
);

// The effective translation catalog behind `t()`. Besides locale switches it
// also swaps when a lazy startup catalog finishes loading while `locale`
// itself stays put, so translation-driven side effects (e.g. imperative DOM
// patches) must track this signal rather than `locale`.
export const effectiveCatalog = computed(() => activeCatalog.value);

function applyLocale(value: Locale): void {
  locale.value = value;
  const catalog = catalogs.get(value);
  if (catalog) activeCatalog.value = catalog;
  writeLocale(localeStorage, value);
}

export function setLocale(value: Locale): Promise<void> {
  const request = ++localeRequest;
  if (catalogs.has(value)) {
    applyLocale(value);
    return Promise.resolve();
  }
  // Lazy locales swap in once their chunk arrives; the UI keeps the previous
  // language until then so text never mixes catalogs mid-translation.
  return ensureLocaleLoaded(value).then(() => {
    if (request === localeRequest && catalogs.has(value)) applyLocale(value);
  });
}

// A stored browser preference may point at a lazy locale; warm it up at startup.
if (!catalogs.has(locale.value)) {
  const initial = locale.value;
  void ensureLocaleLoaded(initial).then(() => {
    if (locale.value === initial) {
      const catalog = catalogs.get(initial);
      if (catalog) activeCatalog.value = catalog;
    }
  });
}

export function t(key: MessageKey, params: TranslationParams = {}): string {
  return activeCatalog.value[key].replace(/\{(\w+)\}/g, (placeholder, name: string) => (
    Object.prototype.hasOwnProperty.call(params, name) ? String(params[name]) : placeholder
  ));
}

watch(locale, (value) => {
  if (typeof document !== "undefined") document.documentElement.lang = value;
}, { immediate: true });
