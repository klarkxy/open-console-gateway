import assert from "node:assert/strict";
import test from "node:test";
import { userFacingError } from "./utils/errors.ts";
import {
  DEFAULT_LOCALE,
  LOCALE_OPTIONS,
  LOCALE_STORAGE_KEY,
  locale,
  matchLocale,
  readLocale,
  resolveLocale,
  setLocale,
  t,
  writeLocale,
} from "./i18n/index.ts";
import type { MessageKey } from "./i18n/index.ts";
import { deDEMessages } from "./i18n/messages/de-DE.ts";
import { enUSMessages } from "./i18n/messages/en-US.ts";
import { esESMessages } from "./i18n/messages/es-ES.ts";
import { frFRMessages } from "./i18n/messages/fr-FR.ts";
import { jaJPMessages } from "./i18n/messages/ja-JP.ts";
import { koKRMessages } from "./i18n/messages/ko-KR.ts";
import { ptBRMessages } from "./i18n/messages/pt-BR.ts";
import { ruRUMessages } from "./i18n/messages/ru-RU.ts";
import { zhTWMessages } from "./i18n/messages/zh-TW.ts";
import { formatCost } from "./utils/format.ts";

const localeCatalogs = {
  "zh-TW": zhTWMessages,
  "en-US": enUSMessages,
  "ja-JP": jaJPMessages,
  "ko-KR": koKRMessages,
  "es-ES": esESMessages,
  "fr-FR": frFRMessages,
  "de-DE": deDEMessages,
  "pt-BR": ptBRMessages,
  "ru-RU": ruRUMessages,
} as const;

test("locale matching uses stored preference, browser languages, and a stable fallback", () => {
  assert.equal(matchLocale("zh-Hant-HK"), "zh-TW");
  assert.equal(matchLocale("pt_PT"), "pt-BR");
  assert.equal(matchLocale("es-MX"), "es-ES");
  assert.equal(resolveLocale("ru_RU", ["en-US"]), "ru-RU");
  assert.equal(resolveLocale("unknown", ["fr-CA", "en-US"]), "fr-FR");
  assert.equal(resolveLocale(null, ["unknown"]), DEFAULT_LOCALE);
});

test("locale preference can be read and written without requiring browser storage", () => {
  const values = new Map<string, string>();
  const storage = {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => { values.set(key, value); },
  };

  writeLocale(storage, "ja-JP");
  assert.equal(values.get(LOCALE_STORAGE_KEY), "ja-JP");
  assert.equal(readLocale(storage, ["en-US"]), "ja-JP");
  assert.equal(readLocale({ getItem: () => { throw new Error("blocked"); } }, ["ko-KR"]), "ko-KR");
});

test("all locale catalogs have identical keys and placeholders", () => {
  const expectedKeys = Object.keys(enUSMessages).sort();
  const placeholders = (value: string) => [...value.matchAll(/\{\w+\}/g)].map(([token]) => token).sort();

  for (const [value, catalog] of Object.entries(localeCatalogs)) {
    assert.deepEqual(Object.keys(catalog).sort(), expectedKeys, value);
    for (const key of expectedKeys) {
      assert.deepEqual(placeholders(catalog[key as MessageKey]), placeholders(key), `${value}: ${key}`);
    }
  }
});

test("translations react to locale changes and preserve interpolation", () => {
  setLocale("en-US");
  assert.equal(t("已复制 {label}", { label: "API Base URL" }), "Copied API Base URL");
  setLocale("zh-CN");
  assert.equal(t("已复制 {label}", { label: "Key" }), "已复制 Key");
});

test("a late lazy locale load cannot override a later locale selection", async () => {
  setLocale("ja-JP");
  setLocale("en-US");

  await new Promise<void>((resolve) => setTimeout(resolve, 0));

  assert.equal(locale.value, "en-US");
  assert.equal(t("已复制 {label}", { label: "Key" }), "Copied Key");
});

test("USD costs use the narrow dollar symbol and preserve requested precision", () => {
  for (const { value } of LOCALE_OPTIONS) {
    setLocale(value);
    assert.match(formatCost(0.00015, 5), /\$/);
    assert.doesNotMatch(formatCost(0.00015, 5), /US/);
  }
  setLocale(DEFAULT_LOCALE);
  assert.match(formatCost(0.00015, 5), /0\.00015/);
  assert.equal(formatCost(-5), "-$5.00");
  assert.equal(formatCost(-0.005), "-$0.0050");
});

test("network failures use a human-facing fallback without hiding server errors", () => {
  assert.equal(userFacingError(new TypeError("Failed to fetch"), "offline"), "offline");
  assert.equal(userFacingError(new TypeError("NetworkError when attempting to fetch resource."), "offline"), "offline");
  assert.equal(userFacingError(new TypeError("Load failed"), "offline"), "offline");
  assert.equal(userFacingError(new Error("server detail"), "offline"), "server detail");
  assert.equal(userFacingError("plain", "offline"), "plain");
  assert.equal(userFacingError(new TypeError("syntax boom"), "offline"), "syntax boom");
});
