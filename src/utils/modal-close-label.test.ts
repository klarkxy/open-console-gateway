import assert from "node:assert/strict";
import test from "node:test";
import { watch } from "vue";
import { setLocale } from "../i18n/index.ts";
import { enUSMessages } from "../i18n/messages/en-US.ts";
import { frFRMessages } from "../i18n/messages/fr-FR.ts";
import { applyModalCloseAriaLabel, modalCloseAriaLabel } from "./modal-close-label.ts";

// The zh-CN catalog renders keys verbatim and every other locale renders its
// catalog value, so expected labels are derived from the message catalogs
// instead of spelled out as literal copy.
const CLOSE_DIALOG_KEY = "关闭对话框" as const;

type StubElement = {
  attrs: Record<string, string>;
  setAttribute(name: string, value: string): void;
};

function stubRoot(elements: StubElement[]) {
  return {
    queried: [] as string[],
    querySelectorAll(selector: string) {
      this.queried.push(selector);
      return elements;
    },
  };
}

function stubElement(attrs: Record<string, string>): StubElement {
  return {
    attrs: { ...attrs },
    setAttribute(name, value) {
      this.attrs[name] = value;
    },
  };
}

test("modal close accessible name is localized, never naive-ui's hardcoded English", () => {
  setLocale("zh-CN");
  const zhLabel = modalCloseAriaLabel();
  assert.equal(zhLabel, CLOSE_DIALOG_KEY, "zh-CN renders the catalog key itself");

  setLocale("en-US");
  const enLabel = modalCloseAriaLabel();
  assert.equal(enLabel, enUSMessages[CLOSE_DIALOG_KEY]);
  assert.notEqual(enLabel, "close", "must never fall back to naive-ui's hardcoded English");
  assert.notEqual(enLabel, zhLabel, "accessible name must follow the active locale");

  setLocale("zh-CN");
});

test("applyModalCloseAriaLabel rewrites naive close buttons scoped to the modal class", () => {
  const elements = [stubElement({ "aria-label": "close" }), stubElement({})];
  const root = stubRoot(elements);

  setLocale("zh-CN");
  applyModalCloseAriaLabel(root as unknown as ParentNode, "account-modal");
  assert.deepEqual(root.queried, [".account-modal .n-base-close"]);
  const zhLabel = modalCloseAriaLabel();
  assert.equal(elements[0].attrs["aria-label"], zhLabel);
  assert.equal(elements[1].attrs["aria-label"], zhLabel);

  // Re-applying after a locale switch keeps the label in the active language.
  setLocale("en-US");
  applyModalCloseAriaLabel(root as unknown as ParentNode, "account-modal");
  const enLabel = modalCloseAriaLabel();
  assert.equal(elements[0].attrs["aria-label"], enLabel);
  assert.notEqual(elements[0].attrs["aria-label"], zhLabel);
  assert.equal(elements[1].attrs["aria-label"], enLabel);
});

test("a stored lazy locale activates its catalog at startup without a locale change", async () => {
  // Simulate app startup with a stored fr-FR locale in a fresh i18n module
  // instance: `locale` starts at fr-FR while the catalog is still the zh-CN
  // fallback, then the lazy warmup swaps the catalog in place. The close-label
  // fix relies on `effectiveCatalog` firing here while `locale` never changes.
  const globals = globalThis as Record<string, unknown>;
  const previousWindow = globals.window;
  globals.window = {
    localStorage: { getItem: () => "fr-FR", setItem: () => {} },
  };
  try {
    const specifier = "../i18n/index.ts?lazy-startup-fr";
    const fresh = (await import(specifier)) as typeof import("../i18n/index.ts");

    assert.equal(fresh.locale.value, "fr-FR");
    // The lazy chunk has not arrived yet, so the catalog is the zh-CN fallback.
    assert.equal(fresh.t(CLOSE_DIALOG_KEY), CLOSE_DIALOG_KEY);

    let localeFires = 0;
    let catalogFires = 0;
    watch(fresh.locale, () => { localeFires += 1; });
    watch(fresh.effectiveCatalog, () => { catalogFires += 1; });

    for (let attempt = 0; attempt < 200 && fresh.t(CLOSE_DIALOG_KEY) === CLOSE_DIALOG_KEY; attempt += 1) {
      await new Promise((resolve) => setTimeout(resolve, 10));
    }

    assert.equal(fresh.locale.value, "fr-FR");
    assert.equal(fresh.t(CLOSE_DIALOG_KEY), frFRMessages[CLOSE_DIALOG_KEY]);
    assert.equal(localeFires, 0, "lazy startup activation must not change locale");
    assert.ok(catalogFires > 0, "effectiveCatalog must fire when the lazy catalog activates");
  } finally {
    if (previousWindow === undefined) {
      delete globals.window;
    } else {
      globals.window = previousWindow;
    }
  }
});
