---
name: Open Console Gateway
colors:
  canvas: "#F4F6FA"
  surface: "#FFFFFF"
  ink: "#1B2430"
  muted: "#566171"
  primary: "#0967D2"
  primary-soft: "#E8F3FF"
  success: "#0B6844"
  warning: "#8A4D00"
  error: "#A92742"
  info: "#245DB6"
typography:
  display: "Segoe UI Variable Text, Segoe UI, Noto Sans SC, sans-serif"
  body: "Segoe UI Variable Text, Segoe UI, Noto Sans SC, Microsoft YaHei UI, sans-serif"
  data: "Cascadia Code, SFMono-Regular, Consolas, Liberation Mono, monospace"
spacing:
  xs: 4
  sm: 8
  md: 12
  lg: 16
  xl: 24
  xxl: 32
rounded:
  small: 6
  medium: 8
  large: 12
---

# Open Console Gateway

## Overview

A calm local operations workspace, not a promotional landing page or a chat clone. The visual reference is the official MoonshotAI Kimi Web UI before its source migration, not a community desktop fork. Keep Vue 3, Naive UI, Pinia, Tauri, OCG branding, and all existing service contracts.

This file is the current **appearance authority**. The previous specification is retained byte-for-byte in [DESIGN.product.md](DESIGN.product.md) so its detailed account, Provider, catalog, Key, CPA, pricing and routing interaction requirements are not lost. Those product requirements remain in effect; its old palette, pure-black/tint requirements, typography, decorative hero and layout styling are superseded here. Treat working source as authoritative where an old description has drifted.

Reference provenance, implementation map and regression checklist: [English](docs/maintainer/ui-design.md) / [简体中文](docs/maintainer/ui-design.zh-CN.md).

## Colors

`src/theme.ts` owns both CSS variables and Naive UI theme overrides. `src/styles/main.css` contains matching first-paint fallbacks. A theme change must reach popovers, dialogs, tables and buttons, not just the page background.

The seven existing choices and stored identifiers remain unchanged. 默认 follows the operating system; all other choices are fixed. 皓白 uses a cool-gray canvas, white panels and blue interactions. 曜黑 uses layered graphite surfaces, not pure black. 藤紫、霁蓝、青瓷、暖铜 keep individually tinted canvas, panels, controls and accents, with lower saturation than the previous design.

Use four surface roles: canvas, surface, raised, sunken. Neutral hover and table headers use sunken, not an accent wash. Selection and active navigation use primary-soft. Primary is for actions, links and keyboard focus; success, warning and error retain their existing meanings. Cooling is warning, not error. Never infer upstream health from configuration state.

The reference blue `#1783FF` is retained as an accent. Necessary small text and filled light-mode actions use `#0967D2` with an explicit `onPrimary` foreground. Dark buttons have an explicit dark foreground. Theme tests require at least 4.5:1 contrast for necessary text on all four surfaces and primary labels in every enabled interaction state. Decorative separators are not form-control boundaries; keep the tested input boundary contrast.

## Typography

Use system sans-serif for UI and headings, and the shared monospace stack for machine-readable data. Do not bundle third-party fonts. The six text steps remain 12/13/14/16/20/24px. Use 14px body, 13px secondary copy, 12px short captions, 16px section titles, 20px page/connection titles and 24px authentication heading. Prefer weight 400/500/600; reserve bold for actual emphasis. Do not replace readable labels with unexplained symbols.

## Layout

Keep the eight core destinations, in order: Dashboard, Access Keys, Accounts, Providers, Aliases, Applications, Logs, Settings. Extensions remain below a divider, with CPA retaining its existing local integration boundary.

The desktop rail is 224px, collapsible to 64px with a persisted UI-only preference. The header is 60px. Content is bounded at 1440px, with 24/32px desktop gutters and smaller phone gutters. Below 1024px retain the existing keyboard-accessible mobile navigation dropdown. Do not squeeze data columns indefinitely; tables retain local horizontal scrolling.

Dashboard order is connection center, attention list, then the full-width daily Token chart. Do not invent KPI data. API and masked Key remain above the fold, with adjacent copy, rotate-current and manage actions. API/Key labels are visible. The mascot is secondary decoration and is hidden on narrower workspaces; it cannot cover controls. Remove the decorative grid and glow.

Ctrl/Command K opens local navigation search from the authenticated shell. Search matches the active-language label and stable view ID, preserves navigation order, supports arrow keys/Enter/Escape, respects IME composition, and does not open over a credential modal. It performs no network operation. Existing KeepAlive view state and URL handling remain intact.

## Shapes

Controls use 6/8px radii, panels 12px, and dialogs/connection surface 16px. Use one low-contrast boundary per surface. Avoid pills, repeated card nesting, gradients, glass, hover lifting and ornamental shadows. Elevation is strongest for actual overlays, not every data row.

## Components

Use Naive UI public theme overrides for Button, Menu, Card, Input, DataTable and Dialog. Do not create a parallel component framework. All account types keep AccountCardFrame's shared header and four action positions. FormSurface preserves a single form body and validation path; long modal bodies scroll without pushing header/footer actions out of the viewport.

Icon utilities retain accessible names and tooltips. Commit and destructive actions retain visible text and existing confirmation. Use the current icon library and OCG/vendor assets, never Kimi's logo. Unknown or missing values remain unknown; no fabricated success states, balances or progress.

## Do's & Don'ts

- Keep the credential name **Key**, redaction, CAS mutations, authentication, logout cache clearing and explicit upstream-test consent.
- Keep Provider/Plan/Custom API/CPA ownership, static adapters, catalog refresh and routing behavior unchanged.
- Use semantic tokens, shared dimensions and component overrides rather than another trailing CSS override layer.
- Keep keyboard focus, reduced motion and native input behavior. Do not remount pages to change appearance.
- Do not automatically refresh upstream catalogs/pricing, probe accounts or check releases as part of a visual change.

## Responsive

Validate at 1440×900, 1280×720, 768×1024 and 390×844. Phone layouts retain primary actions, readable key values and in-pane data scrolling. Modal max-height follows the dynamic viewport with scrollable content. Do not disable browser zoom or hide overflow to conceal clipped controls.

## Iteration Guide

Update theme.ts and first-paint fallbacks together. Run `pnpm run test:web`, `pnpm run build:web` and `pnpm run design:lint`. Inspect light/dark rendering, all fixed themes, keyboard navigation, long translated labels, empty/error/loading states, wide tables and long forms. Report source parsing, unit tests, web build, browser checks and real Tauri use as distinct evidence; a green syntax check is not a visual review.
