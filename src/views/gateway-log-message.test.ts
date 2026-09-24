import assert from "node:assert/strict";
import test from "node:test";
import { locale, setLocale, type Locale } from "../i18n/index.ts";
import { gatewayLogMessage } from "./gateway-log-message.ts";

// Lazy catalogs swap in asynchronously; poll until the selection sticks.
async function waitForLocale(value: Locale): Promise<void> {
  setLocale(value);
  for (let i = 0; i < 200 && locale.value !== value; i += 1) {
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
  assert.equal(locale.value, value);
}

test("localizes the known account-created runtime log message", () => {
  const name = "UI smoke Custom draft";
  const source = `created account ${name}`;
  setLocale("zh-CN");
  const zh = gatewayLogMessage(source);
  // Known events are localized, never passed through, and keep the account name.
  assert.notEqual(zh, source);
  assert.ok(zh.includes(name));
  assert.ok(!zh.includes("{name}"));
  setLocale("en-US");
  const en = gatewayLogMessage(source);
  assert.notEqual(en, source);
  assert.ok(en.includes(name));
  assert.ok(!en.includes("{name}"));
  assert.notEqual(en, zh);
});

test("interpolates the account name verbatim, including placeholder-like text", () => {
  setLocale("en-US");
  const placeholderLike = "{name} <b>smoke</b>";
  const source = `created account ${placeholderLike}`;
  const rendered = gatewayLogMessage(source);
  assert.notEqual(rendered, source);
  assert.ok(rendered.endsWith(placeholderLike));
  const unicodeName = "主号（生产）";
  assert.ok(gatewayLogMessage(`created account ${unicodeName}`).endsWith(unicodeName));
});

test("renders the account-created message in a non-English lazy locale", async () => {
  const source = "created account UI smoke Custom draft";
  await waitForLocale("fr-FR");
  const french = gatewayLogMessage(source);
  assert.notEqual(french, source);
  assert.ok(french.includes("UI smoke Custom draft"));
  await waitForLocale("ja-JP");
  const japanese = gatewayLogMessage(source);
  assert.notEqual(japanese, source);
  assert.ok(japanese.includes("UI smoke Custom draft"));
});

test("passes unknown backend log strings through untouched", () => {
  setLocale("zh-CN");
  assert.equal(gatewayLogMessage("upstream 429 too many requests"), "upstream 429 too many requests");
  assert.equal(gatewayLogMessage("created something else entirely"), "created something else entirely");
  // `created account` without a name is not the known event; keep it as-is.
  assert.equal(gatewayLogMessage("created account"), "created account");
  setLocale("en-US");
  assert.equal(gatewayLogMessage("stream aborted: client disconnected"), "stream aborted: client disconnected");
});
