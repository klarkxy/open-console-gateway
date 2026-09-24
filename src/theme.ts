import type { GlobalThemeOverrides } from "naive-ui";

export type ThemeName = "default" | "white" | "black" | "violet" | "azure" | "celadon" | "copper";
export type ResolvedTheme = Exclude<ThemeName, "default">;

export interface ThemeTokens {
  colorScheme: "light" | "dark";
  canvas: string;
  surface: string;
  surfaceRaised: string;
  surfaceSunken: string;
  ink: string;
  muted: string;
  subtle: string;
  accent: string;
  primary: string;
  primaryHover: string;
  primaryPressed: string;
  primarySoft: string;
  onPrimary: string;
  success: string;
  successSoft: string;
  warning: string;
  warningSoft: string;
  error: string;
  info: string;
  border: string;
  divider: string;
  shadowSm: string;
  shadowLg: string;
  mascotHalo: string;
  mascotRim: string;
}

export const THEME_STORAGE_KEY = "ocg-manager.theme";

export const THEME_OPTIONS: ReadonlyArray<{ value: ThemeName; label: string; swatch: string }> = [
  { value: "default", label: "默认", swatch: "linear-gradient(135deg, #f4f6fa 0 50%, #0d1117 50%)" },
  { value: "white", label: "皓白", swatch: "#F4F6FA" },
  { value: "black", label: "曜黑", swatch: "#0D1117" },
  { value: "violet", label: "藤紫", swatch: "#5B44B4" },
  { value: "azure", label: "霁蓝", swatch: "#0967D2" },
  { value: "celadon", label: "青瓷", swatch: "#0B666B" },
  { value: "copper", label: "暖铜", swatch: "#8A4F34" },
];

/** Kimi Web's official pre-migration design system is the visual reference.
 * Palette provenance and intentional accessibility changes: docs/maintainer/ui-design.md.
 * Runtime styles and Naive UI must consume this same source, including portals.
 */
const lightSemantic = {
  colorScheme: "light",
  onPrimary: "#FFFFFF",
  success: "#0B6844",
  successSoft: "#E6F4EE",
  warning: "#8A4D00",
  warningSoft: "#FFF1D8",
  error: "#A92742",
  info: "#245DB6",
  shadowSm: "0 1px 2px rgba(28, 40, 66, 0.04)",
  shadowLg: "0 8px 24px rgba(28, 40, 66, 0.08), 0 24px 64px rgba(28, 40, 66, 0.12)",
  mascotRim: "transparent",
} as const;

export const THEME_TOKENS: Record<ResolvedTheme, ThemeTokens> = {
  white: {
    ...lightSemantic,
    canvas: "#F4F6FA",
    surface: "#FFFFFF",
    surfaceRaised: "#FFFFFF",
    surfaceSunken: "#F3F5F8",
    ink: "#1B2430",
    muted: "#566171",
    subtle: "#657080",
    accent: "#1783FF",
    // The brand blue is used for focus/decoration. Small text and filled
    // actions use a darker shade so white labels meet 4.5:1 contrast.
    primary: "#0967D2",
    primaryHover: "#085AB8",
    primaryPressed: "#064999",
    primarySoft: "#E8F3FF",
    border: "#DFE5ED",
    divider: "#E9EDF3",
    mascotHalo: "rgba(23, 131, 255, 0.06)",
  },
  black: {
    colorScheme: "dark",
    canvas: "#0D1117",
    surface: "#161B22",
    surfaceRaised: "#1C2128",
    surfaceSunken: "#10151C",
    ink: "#E6EDF3",
    muted: "#ABB5C3",
    subtle: "#97A3B4",
    accent: "#58A6FF",
    primary: "#79B8FF",
    primaryHover: "#A6D1FF",
    primaryPressed: "#58A6FF",
    primarySoft: "#1C2A3A",
    onPrimary: "#0D1117",
    success: "#56C596",
    successSoft: "#18372C",
    warning: "#E7AE55",
    warningSoft: "#3C2E18",
    error: "#F08095",
    info: "#79B8FF",
    border: "#303842",
    divider: "#262D36",
    shadowSm: "0 1px 2px rgba(0, 0, 0, 0.16)",
    shadowLg: "0 8px 24px rgba(0, 0, 0, 0.24), 0 24px 64px rgba(0, 0, 0, 0.40)",
    mascotHalo: "rgba(88, 166, 255, 0.08)",
    mascotRim: "rgba(255, 255, 255, 0.28)",
  },
  violet: {
    ...lightSemantic,
    canvas: "#F1EEF7",
    surface: "#FAF8FD",
    surfaceRaised: "#FDFBFF",
    surfaceSunken: "#EDE8F4",
    ink: "#211A2D",
    muted: "#51475F",
    subtle: "#685D77",
    accent: "#8065D6",
    primary: "#5B44B4",
    primaryHover: "#4F399F",
    primaryPressed: "#402B89",
    primarySoft: "#E9E1F8",
    border: "#DAD1E9",
    divider: "#E7E0F0",
    mascotHalo: "rgba(91, 68, 180, 0.08)",
  },
  azure: {
    ...lightSemantic,
    canvas: "#ECF3FA",
    surface: "#F7FBFF",
    surfaceRaised: "#FBFDFF",
    surfaceSunken: "#E7EFF8",
    ink: "#172435",
    muted: "#46586E",
    subtle: "#52647B",
    accent: "#1783FF",
    primary: "#0967D2",
    primaryHover: "#085AB8",
    primaryPressed: "#064999",
    primarySoft: "#DDEDFE",
    border: "#CEDFEE",
    divider: "#DFEAF5",
    mascotHalo: "rgba(23, 131, 255, 0.08)",
  },
  celadon: {
    ...lightSemantic,
    canvas: "#EDF5F1",
    surface: "#F7FCF9",
    surfaceRaised: "#FBFEFC",
    surfaceSunken: "#E6F0EB",
    ink: "#172721",
    muted: "#435C52",
    subtle: "#4F665C",
    accent: "#188B82",
    primary: "#0B666B",
    primaryHover: "#09595D",
    primaryPressed: "#074A4E",
    primarySoft: "#DBEEE6",
    border: "#CDDED5",
    divider: "#DFECE5",
    mascotHalo: "rgba(11, 102, 107, 0.08)",
  },
  copper: {
    ...lightSemantic,
    canvas: "#F6F0EA",
    surface: "#FDF9F5",
    surfaceRaised: "#FFFCF9",
    surfaceSunken: "#EFE7DF",
    ink: "#30221B",
    muted: "#664B3E",
    subtle: "#745A4E",
    accent: "#B47755",
    primary: "#8A4F34",
    primaryHover: "#78432C",
    primaryPressed: "#653824",
    primarySoft: "#F2E4D8",
    border: "#E1D2C5",
    divider: "#EDE2D8",
    mascotHalo: "rgba(138, 79, 52, 0.08)",
  },
};

export const CHART_PALETTE = [
  "var(--ocg-primary)", "#8B6CCF", "#16845B", "#A85F00",
  "#C33B55", "#0F8C91", "#7B7987", "#A454B8",
] as const;

/** Shared dimensions, including CSS fallbacks and component overrides. */
export const DESIGN_TOKENS = {
  "--ocg-font-ui": '-apple-system, BlinkMacSystemFont, "Segoe UI Variable Text", "Segoe UI", "Noto Sans SC", "Microsoft YaHei UI", sans-serif',
  "--ocg-font-mono": '"Cascadia Code", "SFMono-Regular", Consolas, "Liberation Mono", monospace',
  "--ocg-font-xs": "12px", "--ocg-font-sm": "13px", "--ocg-font-md": "14px",
  "--ocg-font-lg": "16px", "--ocg-font-xl": "20px", "--ocg-font-2xl": "24px",
  "--ocg-space-xs": "4px", "--ocg-space-sm": "8px", "--ocg-space-md": "12px",
  "--ocg-space-lg": "16px", "--ocg-space-xl": "24px", "--ocg-space-2xl": "32px",
  "--ocg-radius-sm": "6px", "--ocg-radius-md": "8px", "--ocg-radius-lg": "12px", "--ocg-radius-xl": "16px",
  "--ocg-motion-fast": "120ms", "--ocg-motion-normal": "180ms",
  "--ocg-ease": "cubic-bezier(0.2, 0, 0, 1)",
  "--ocg-header-height": "60px", "--ocg-content-max": "1440px",
} as const;

const themeNames = new Set<ThemeName>(THEME_OPTIONS.map(({ value }) => value));

export function readTheme(storage: Pick<Storage, "getItem"> | null): ThemeName {
  let value: string | null | undefined;
  try { value = storage?.getItem(THEME_STORAGE_KEY); } catch { return "default"; }
  if (value === "system") return "default";
  if (value === "light") return "white";
  if (value === "dark") return "black";
  return value && themeNames.has(value as ThemeName) ? value as ThemeName : "default";
}

export function getThemeStorage(): Storage | null {
  if (typeof window === "undefined") return null;
  try { return window.localStorage; } catch { return null; }
}

export function writeTheme(storage: Pick<Storage, "setItem"> | null, theme: ThemeName): void {
  try { storage?.setItem(THEME_STORAGE_KEY, theme); } catch {
    // Private/locked-down browsers can reject persistence; the active theme still works.
  }
}

export function resolveTheme(theme: ThemeName, osTheme: string | null | undefined): ResolvedTheme {
  return theme === "default" ? (osTheme === "dark" ? "black" : "white") : theme;
}

export function getThemeTokens(theme: ThemeName, osTheme: string | null | undefined): ThemeTokens {
  return THEME_TOKENS[resolveTheme(theme, osTheme)];
}

export function themeCssVariables(tokens: ThemeTokens): Record<string, string> {
  return {
    ...DESIGN_TOKENS,
    "--ocg-canvas": tokens.canvas, "--ocg-surface": tokens.surface,
    "--ocg-surface-raised": tokens.surfaceRaised, "--ocg-surface-sunken": tokens.surfaceSunken,
    "--ocg-ink": tokens.ink, "--ocg-muted": tokens.muted, "--ocg-subtle": tokens.subtle,
    "--ocg-accent": tokens.accent, "--ocg-primary": tokens.primary,
    "--ocg-primary-hover": tokens.primaryHover, "--ocg-primary-pressed": tokens.primaryPressed,
    "--ocg-primary-soft": tokens.primarySoft, "--ocg-on-primary": tokens.onPrimary,
    "--ocg-success": tokens.success, "--ocg-success-soft": tokens.successSoft,
    "--ocg-warning": tokens.warning, "--ocg-warning-soft": tokens.warningSoft,
    "--ocg-error": tokens.error, "--ocg-info": tokens.info,
    "--ocg-border": tokens.border, "--ocg-divider": tokens.divider,
    "--ocg-shadow-sm": tokens.shadowSm, "--ocg-shadow-lg": tokens.shadowLg,
    "--ocg-mascot-halo": tokens.mascotHalo, "--ocg-mascot-rim": tokens.mascotRim,
  };
}

export function applyTheme(root: HTMLElement, resolved: ResolvedTheme, tokens: ThemeTokens): void {
  root.dataset.theme = resolved;
  root.style.colorScheme = tokens.colorScheme;
  for (const [name, value] of Object.entries(themeCssVariables(tokens))) root.style.setProperty(name, value);
}

export function toNaiveThemeOverrides(tokens: ThemeTokens): GlobalThemeOverrides {
  const { surface, surfaceRaised, surfaceSunken, ink, muted, primary, primarySoft } = tokens;
  return {
    common: {
      bodyColor: tokens.canvas, cardColor: surface, modalColor: surfaceRaised,
      popoverColor: surfaceRaised, tableColor: surface, inputColor: surfaceRaised,
      actionColor: surfaceSunken, tableHeaderColor: surfaceSunken,
      hoverColor: surfaceSunken, pressedColor: tokens.canvas,
      primaryColor: primary, primaryColorHover: tokens.primaryHover,
      primaryColorPressed: tokens.primaryPressed, primaryColorSuppl: tokens.primaryHover,
      textColorBase: ink, textColor1: ink, textColor2: muted, textColor3: tokens.subtle,
      successColor: tokens.success, warningColor: tokens.warning, errorColor: tokens.error, infoColor: tokens.info,
      borderColor: tokens.border, dividerColor: tokens.divider,
      borderRadius: DESIGN_TOKENS["--ocg-radius-md"],
      fontFamily: DESIGN_TOKENS["--ocg-font-ui"], fontFamilyMono: DESIGN_TOKENS["--ocg-font-mono"],
      fontSize: "14px", fontSizeMini: "12px", fontSizeTiny: "12px", fontSizeSmall: "13px",
      fontSizeMedium: "14px", fontSizeLarge: "16px", fontSizeHuge: "20px", lineHeight: "1.6",
    },
    Button: {
      borderRadiusTiny: "6px", borderRadiusSmall: "6px", borderRadiusMedium: "8px", borderRadiusLarge: "8px",
      textColorPrimary: tokens.onPrimary, textColorHoverPrimary: tokens.onPrimary,
      textColorPressedPrimary: tokens.onPrimary, textColorFocusPrimary: tokens.onPrimary,
      fontWeight: "500",
    },
    Card: {
      borderRadius: "12px", titleFontWeight: "600", titleFontSizeSmall: "14px",
      titleFontSizeMedium: "16px", paddingSmall: "16px", paddingMedium: "24px",
      boxShadow: tokens.shadowSm,
    },
    Menu: {
      itemHeight: "38px", borderRadius: "8px", fontSize: "14px",
      itemColorHover: surfaceSunken, itemColorActive: primarySoft,
      itemColorActiveHover: primarySoft, itemColorActiveCollapsed: primarySoft,
      itemTextColor: muted, itemTextColorHover: ink, itemTextColorActive: primary,
      itemTextColorActiveHover: primary, itemIconColor: muted,
      itemIconColorHover: ink, itemIconColorActive: primary, itemIconColorActiveHover: primary,
    },
    Input: {
      border: `1px solid color-mix(in srgb, ${muted} 68%, ${surfaceRaised})`,
      borderRadius: "8px",
    },
    DataTable: {
      thColor: surfaceSunken, thTextColor: muted, thFontWeight: "500",
      tdColor: surface, tdColorHover: surfaceSunken, borderColor: tokens.divider,
      borderRadius: "12px",
    },
    Dialog: { borderRadius: "16px" },
  };
}
