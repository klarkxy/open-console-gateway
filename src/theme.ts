import type { GlobalThemeOverrides } from "naive-ui";

export type ThemeName = "default" | "white" | "black" | "violet" | "azure" | "celadon" | "copper";
export type ResolvedTheme = Exclude<ThemeName, "default">;

export interface ThemeTokens {
  colorScheme: "light" | "dark";
  canvas: string;
  surface: string;
  surfaceRaised: string;
  ink: string;
  muted: string;
  subtle: string;
  primary: string;
  primaryHover: string;
  primaryPressed: string;
  primarySoft: string;
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
  { value: "default", label: "默认", swatch: "linear-gradient(135deg, #fff 0 50%, #000 50%)" },
  { value: "white", label: "皓白", swatch: "#FFFFFF" },
  { value: "black", label: "曜黑", swatch: "#000000" },
  { value: "violet", label: "藤紫", swatch: "#5B44B4" },
  { value: "azure", label: "霁蓝", swatch: "#0F50E5" },
  { value: "celadon", label: "青瓷", swatch: "#0B666B" },
  { value: "copper", label: "暖铜", swatch: "#8A4F34" },
];

const lightSemantic = {
  success: "#0B6844",
  successSoft: "#E6F4EE",
  warning: "#8A4D00",
  warningSoft: "#FFF1D8",
  error: "#A92742",
  info: "#245DB6",
  shadowSm: "0 1px 2px rgba(24, 24, 32, 0.05)",
  shadowLg: "0 22px 60px rgba(35, 30, 66, 0.12)",
} as const;

export const THEME_TOKENS: Record<ResolvedTheme, ThemeTokens> = {
  white: {
    colorScheme: "light",
    canvas: "#F7F7F8",
    surface: "#FFFFFF",
    surfaceRaised: "#FFFFFF",
    ink: "#18181B",
    muted: "#5F6068",
    subtle: "#6F6F78",
    primary: "#18181B",
    primaryHover: "#303038",
    primaryPressed: "#050506",
    primarySoft: "#ECECEF",
    border: "#E3E3E8",
    divider: "#E9E9ED",
    mascotHalo: "rgba(24, 24, 27, 0.06)",
    mascotRim: "transparent",
    ...lightSemantic,
  },
  black: {
    colorScheme: "dark",
    canvas: "#000000",
    surface: "#0B0B0D",
    surfaceRaised: "#151519",
    ink: "#F5F5F7",
    muted: "#B6B3BF",
    subtle: "#8C8994",
    primary: "#F5F5F7",
    primaryHover: "#FFFFFF",
    primaryPressed: "#D6D6DC",
    primarySoft: "#1D1D22",
    success: "#56C596",
    successSoft: "#18372C",
    warning: "#E7AE55",
    warningSoft: "#3C2E18",
    error: "#F08095",
    info: "#74A6F6",
    border: "#28272F",
    divider: "#222228",
    shadowSm: "none",
    shadowLg: "0 22px 60px rgba(0, 0, 0, 0.60)",
    mascotHalo: "rgba(255, 255, 255, 0.14)",
    mascotRim: "rgba(255, 255, 255, 0.48)",
  },
  violet: {
    colorScheme: "light",
    canvas: "#E5DBF0",
    surface: "#EEE5F6",
    surfaceRaised: "#F8F3FC",
    ink: "#211A2D",
    muted: "#51475F",
    subtle: "#685D77",
    primary: "#5B44B4",
    primaryHover: "#6C55C7",
    primaryPressed: "#49349A",
    primarySoft: "#D9CBEF",
    border: "#BFAAD5",
    divider: "#D3C5E2",
    mascotHalo: "rgba(91, 68, 180, 0.18)",
    mascotRim: "transparent",
    ...lightSemantic,
  },
  azure: {
    colorScheme: "light",
    canvas: "#D2E3F2",
    surface: "#DEEBF7",
    surfaceRaised: "#EEF5FB",
    ink: "#172435",
    muted: "#46586E",
    subtle: "#52647B",
    primary: "#0F50E5",
    primaryHover: "#2A61E6",
    primaryPressed: "#043DBF",
    primarySoft: "#C9DCF3",
    border: "#A9C2DB",
    divider: "#C3D5E6",
    mascotHalo: "rgba(20, 86, 240, 0.16)",
    mascotRim: "transparent",
    ...lightSemantic,
  },
  celadon: {
    colorScheme: "light",
    canvas: "#D2E5DC",
    surface: "#DDECE5",
    surfaceRaised: "#EEF6F2",
    ink: "#172721",
    muted: "#435C52",
    subtle: "#4F665C",
    primary: "#0B666B",
    primaryHover: "#127277",
    primaryPressed: "#075358",
    primarySoft: "#C4DED4",
    border: "#A4C6B7",
    divider: "#BFD8CD",
    mascotHalo: "rgba(15, 116, 121, 0.16)",
    mascotRim: "transparent",
    ...lightSemantic,
  },
  copper: {
    colorScheme: "light",
    canvas: "#E9D7C8",
    surface: "#F2E5DA",
    surfaceRaised: "#FAF2EB",
    ink: "#30221B",
    muted: "#664B3E",
    subtle: "#745A4E",
    primary: "#8A4F34",
    primaryHover: "#9A5D41",
    primaryPressed: "#6F3F2A",
    primarySoft: "#E4C6B2",
    border: "#CAA58E",
    divider: "#DDC0AD",
    mascotHalo: "rgba(138, 79, 52, 0.18)",
    mascotRim: "transparent",
    ...lightSemantic,
  },
};

export const CHART_PALETTE = [
  "var(--ocg-primary)",
  "#2F6FD4",
  "#16845B",
  "#A85F00",
  "#C33B55",
  "#0F8C91",
  "#7B7987",
  "#A454B8",
] as const;

const themeNames = new Set<ThemeName>(THEME_OPTIONS.map(({ value }) => value));

export function readTheme(storage: Pick<Storage, "getItem"> | null): ThemeName {
  let value: string | null | undefined;
  try {
    value = storage?.getItem(THEME_STORAGE_KEY);
  } catch {
    return "default";
  }
  if (value === "system") return "default";
  if (value === "light") return "white";
  if (value === "dark") return "black";
  return value && themeNames.has(value as ThemeName) ? value as ThemeName : "default";
}

export function getThemeStorage(): Storage | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

export function writeTheme(storage: Pick<Storage, "setItem"> | null, theme: ThemeName): void {
  try {
    storage?.setItem(THEME_STORAGE_KEY, theme);
  } catch {
    // A private or locked-down browser may reject persistence; the in-memory theme still works.
  }
}

export function resolveTheme(theme: ThemeName, osTheme: string | null | undefined): ResolvedTheme {
  return theme === "default" ? (osTheme === "dark" ? "black" : "white") : theme;
}

export function getThemeTokens(theme: ThemeName, osTheme: string | null | undefined): ThemeTokens {
  return THEME_TOKENS[resolveTheme(theme, osTheme)];
}

export function applyTheme(root: HTMLElement, resolved: ResolvedTheme, tokens: ThemeTokens): void {
  root.dataset.theme = resolved;
  root.style.colorScheme = tokens.colorScheme;
  // 字号阶梯：层级由六档字号与字重共同承担（见 DESIGN.md Typography）。
  // 收口为 token 后，若需整体调整字号只需改这一处。
  const values: Record<string, string> = {
    "--ocg-font-xs": "12px",
    "--ocg-font-sm": "13px",
    "--ocg-font-md": "14px",
    "--ocg-font-lg": "16px",
    "--ocg-font-xl": "20px",
    "--ocg-font-2xl": "24px",
    // 间距与圆角刻度（见 DESIGN.md Layout / Shapes），与主题无关，各主题共用。
    "--ocg-space-xs": "4px",
    "--ocg-space-sm": "8px",
    "--ocg-space-md": "12px",
    "--ocg-space-lg": "16px",
    "--ocg-space-xl": "24px",
    "--ocg-space-2xl": "32px",
    "--ocg-radius-sm": "6px",
    "--ocg-radius-md": "10px",
    "--ocg-radius-lg": "14px",
    "--ocg-canvas": tokens.canvas,
    "--ocg-surface": tokens.surface,
    "--ocg-surface-raised": tokens.surfaceRaised,
    "--ocg-ink": tokens.ink,
    "--ocg-muted": tokens.muted,
    "--ocg-subtle": tokens.subtle,
    "--ocg-primary": tokens.primary,
    "--ocg-primary-soft": tokens.primarySoft,
    "--ocg-success": tokens.success,
    "--ocg-success-soft": tokens.successSoft,
    "--ocg-warning": tokens.warning,
    "--ocg-warning-soft": tokens.warningSoft,
    "--ocg-error": tokens.error,
    "--ocg-info": tokens.info,
    "--ocg-border": tokens.border,
    "--ocg-divider": tokens.divider,
    "--ocg-shadow-sm": tokens.shadowSm,
    "--ocg-shadow-lg": tokens.shadowLg,
    "--ocg-mascot-halo": tokens.mascotHalo,
    "--ocg-mascot-rim": tokens.mascotRim,
  };
  for (const [name, value] of Object.entries(values)) root.style.setProperty(name, value);
}

export function toNaiveThemeOverrides(tokens: ThemeTokens): GlobalThemeOverrides {
  return {
    common: {
      bodyColor: tokens.canvas,
      cardColor: tokens.surface,
      modalColor: tokens.surfaceRaised,
      popoverColor: tokens.surfaceRaised,
      tableColor: tokens.surface,
      inputColor: tokens.surfaceRaised,
      actionColor: tokens.primarySoft,
      tableHeaderColor: tokens.primarySoft,
      hoverColor: tokens.primarySoft,
      pressedColor: tokens.canvas,
      primaryColor: tokens.primary,
      primaryColorHover: tokens.primaryHover,
      primaryColorPressed: tokens.primaryPressed,
      primaryColorSuppl: tokens.primaryHover,
      textColorBase: tokens.ink,
      textColor1: tokens.ink,
      textColor2: tokens.muted,
      textColor3: tokens.subtle,
      successColor: tokens.success,
      warningColor: tokens.warning,
      errorColor: tokens.error,
      infoColor: tokens.info,
      borderColor: tokens.border,
      dividerColor: tokens.divider,
      borderRadius: "10px",
      fontFamily: '"Segoe UI Variable Text", "Noto Sans SC", "Microsoft YaHei UI", sans-serif',
      fontFamilyMono: '"Cascadia Mono", Consolas, monospace',
      fontSize: "14px",
      // 字号阶梯（对应 --ocg-font-* token）：层级由字号与字重共同承担，
      // 12/13 用于说明与辅助文本，14 为正文基准，16/20 用于标题与关键数字。
      fontSizeMini: "12px",
      fontSizeTiny: "12px",
      fontSizeSmall: "13px",
      fontSizeMedium: "14px",
      fontSizeLarge: "16px",
      fontSizeHuge: "20px",
      lineHeight: "1.6",
    },
    Input: {
      border: `1px solid color-mix(in srgb, ${tokens.muted} 68%, ${tokens.surfaceRaised})`,
    },
  };
}
