import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const template = readFileSync(
  new URL("../src-tauri/windows/installer.nsi", import.meta.url),
  "utf8",
);
const tauriConf = JSON.parse(
  readFileSync(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8"),
);

test("Windows NSIS template keeps the Tauri interpolations the bundler fills", () => {
  for (const token of [
    "{{compression}}",
    "{{product_name}}",
    "{{main_binary_name}}",
    "{{main_binary_path}}",
    "{{#each resources}}",
    "{{#each resources_dirs}}",
    "{{#if installer_hooks}}",
    "{{#each languages}}",
    "{{#each language_files}}",
    "NSIS_HOOK_PREINSTALL",
    "NSIS_HOOK_POSTINSTALL",
    "NSIS_HOOK_PREUNINSTALL",
    "NSIS_HOOK_POSTUNINSTALL",
  ]) {
    assert.match(template, new RegExp(token.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")));
  }
});

test("Windows NSIS template replaces in place and never nests an uninstaller", () => {
  assert.match(template, /Vendored from tauri-cli 2\.11\.4/);
  assert.match(template, /!define LEGACYPRODUCTNAME "OCG Manager"/);
  assert.match(template, /SkipIfPassiveOrExisting/);
  assert.ok(template.includes('RMDir /r "$INSTDIR\\dist"'));
  assert.ok(template.includes('RmDir /r "$PROFILE\\.ocg-mgr"'));
  assert.equal(template.includes("Page custom PageReinstall"), false);
  assert.equal(template.includes("reinst_uninstall:"), false);
});

test("Tauri config points at the vendored NSIS template and language files", () => {
  const nsis = tauriConf.bundle.windows.nsis;
  assert.equal(nsis.template, "windows/installer.nsi");
  assert.equal(nsis.installerHooks, "installer.nsh");
  assert.deepEqual(nsis.languages, ["English", "SimpChinese"]);
  assert.equal(nsis.displayLanguageSelector, false);
  assert.equal(nsis.customLanguageFiles.English, "windows/languages/English.nsh");
  assert.equal(
    nsis.customLanguageFiles.SimpChinese,
    "windows/languages/SimpChinese.nsh",
  );
});
