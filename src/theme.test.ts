import assert from "node:assert/strict";
import test from "node:test";
import {
  applyTheme, DESIGN_TOKENS, getThemeStorage, getThemeTokens, readTheme,
  resolveTheme, THEME_OPTIONS, THEME_STORAGE_KEY, THEME_TOKENS,
  themeCssVariables, toNaiveThemeOverrides, writeTheme,
} from "./theme.ts";

function storage(value: string | null) {
  return { getItem: (key: string) => key === THEME_STORAGE_KEY ? value : null };
}
function luminance(hex: string): number {
  const rgb = [1, 3, 5].map((start) => Number.parseInt(hex.slice(start, start + 2), 16) / 255)
    .map((c) => c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
  return 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
}
function contrast(a: string, b: string): number {
  const [lighter, darker] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (lighter + 0.05) / (darker + 0.05);
}
function mixHex(a: string, b: string, weight: number): string {
  return `#${[1, 3, 5].map((start) => Math.round(
    Number.parseInt(a.slice(start, start + 2), 16) * weight
    + Number.parseInt(b.slice(start, start + 2), 16) * (1 - weight),
  ).toString(16).padStart(2, "0")).join("")}`;
}

test("saved themes and legacy preferences survive the visual refresh", () => {
  for (const { value } of THEME_OPTIONS) assert.equal(readTheme(storage(value)), value);
  assert.equal(readTheme(storage("system")), "default");
  assert.equal(readTheme(storage("light")), "white");
  assert.equal(readTheme(storage("dark")), "black");
  assert.equal(readTheme(storage("legacy")), "default");
  assert.equal(readTheme(null), "default");
  assert.equal(readTheme({ getItem: () => { throw new Error("blocked"); } }), "default");
  assert.equal(getThemeStorage(), null);
  assert.doesNotThrow(() => writeTheme({ setItem: () => { throw new Error("blocked"); } }, "black"));
  const writes = new Map<string, string>();
  writeTheme({ setItem: (key, value) => { writes.set(key, value); } }, "azure");
  assert.equal(writes.get(THEME_STORAGE_KEY), "azure");
});

test("only the default theme follows the operating system", () => {
  assert.equal(resolveTheme("default", "light"), "white");
  assert.equal(resolveTheme("default", "dark"), "black");
  assert.equal(resolveTheme("default", null), "white");
  for (const theme of Object.keys(THEME_TOKENS) as Array<keyof typeof THEME_TOKENS>) {
    assert.equal(resolveTheme(theme, "dark"), theme);
    assert.equal(resolveTheme(theme, "light"), theme);
  }
});

test("CSS and component themes agree across every theme switch, including dark button labels", () => {
  const properties = new Map<string, string>();
  const root = {
    dataset: {},
    style: { colorScheme: "", setProperty: (name: string, value: string) => properties.set(name, value) },
  } as unknown as HTMLElement;
  for (const resolved of Object.keys(THEME_TOKENS) as Array<keyof typeof THEME_TOKENS>) {
    const tokens = getThemeTokens(resolved, "light");
    applyTheme(root, resolved, tokens);
    const overrides = toNaiveThemeOverrides(tokens);
    const common = overrides.common!;
    assert.equal(root.dataset.theme, resolved);
    assert.equal(root.style.colorScheme, tokens.colorScheme);
    assert.deepEqual(Object.fromEntries(properties), themeCssVariables(tokens));
    assert.equal(properties.get("--ocg-surface-sunken"), tokens.surfaceSunken);
    assert.equal(properties.get("--ocg-primary"), common.primaryColor);
    assert.equal(properties.get("--ocg-on-primary"), overrides.Button!.textColorPrimary);
    assert.equal(properties.get("--ocg-font-ui"), common.fontFamily);
    assert.equal(common.bodyColor, tokens.canvas);
    assert.equal(common.inputColor, tokens.surfaceRaised);
    assert.equal(common.actionColor, tokens.surfaceSunken);
    assert.equal(common.tableHeaderColor, tokens.surfaceSunken);
    assert.equal(common.hoverColor, tokens.surfaceSunken);
    assert.equal(common.borderRadius, DESIGN_TOKENS["--ocg-radius-md"]);
    assert.equal(overrides.Menu!.itemColorActive, tokens.primarySoft);
    assert.equal(overrides.DataTable!.tdColor, tokens.surface);
    for (const key of ["textColorPrimary", "textColorHoverPrimary", "textColorPressedPrimary", "textColorFocusPrimary"] as const) {
      assert.equal(overrides.Button![key], tokens.onPrimary);
    }
    assert.ok(contrast(mixHex(tokens.muted, tokens.surfaceRaised, 0.68), tokens.surfaceRaised) >= 3, `${resolved} input boundary`);
  }
});

test("fixed colored themes retain their own tinted surfaces and dark mode has distinct elevations", () => {
  for (const name of ["violet", "azure", "celadon", "copper"] as const) {
    const tokens = THEME_TOKENS[name];
    assert.notEqual(tokens.canvas, THEME_TOKENS.white.canvas);
    assert.notEqual(tokens.surface, THEME_TOKENS.white.surface);
    assert.ok(luminance(tokens.surfaceRaised) > luminance(tokens.canvas), `${name} raised surface`);
  }
  const dark = THEME_TOKENS.black;
  assert.ok(luminance(dark.canvas) < luminance(dark.surface));
  assert.ok(luminance(dark.surface) < luminance(dark.surfaceRaised));
});

test("necessary text and primary actions meet AA contrast on every surface", () => {
  for (const [name, tokens] of Object.entries(THEME_TOKENS)) {
    for (const background of [tokens.canvas, tokens.surface, tokens.surfaceRaised, tokens.surfaceSunken]) {
      for (const [role, color] of Object.entries({
        ink: tokens.ink, muted: tokens.muted, subtle: tokens.subtle, primary: tokens.primary,
        success: tokens.success, warning: tokens.warning, error: tokens.error, info: tokens.info,
      })) {
        assert.ok(contrast(color, background) >= 4.5, `${name} ${role} on ${background}: ${contrast(color, background).toFixed(2)}`);
      }
    }
    for (const color of [tokens.primary, tokens.primaryHover, tokens.primaryPressed]) {
      assert.ok(contrast(color, tokens.onPrimary) >= 4.5, `${name} button`);
    }
    assert.ok(contrast(tokens.success, tokens.successSoft) >= 4.5, `${name} success soft`);
    assert.ok(contrast(tokens.warning, tokens.warningSoft) >= 4.5, `${name} warning soft`);
    assert.ok(contrast(tokens.primary, tokens.primarySoft) >= 4.5, `${name} selected navigation`);
  }
});
