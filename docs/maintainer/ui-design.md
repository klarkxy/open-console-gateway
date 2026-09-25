# Console UI design

[简体中文](ui-design.zh-CN.md)

## Reference and scope

Reference the **official** `MoonshotAI/kimi-code` repository at `e7d5a0aee74e7f116cca0273c416ece9139a78a0`, specifically [`apps/kimi-web/src/style.css`](https://github.com/MoonshotAI/kimi-code/blob/e7d5a0aee74e7f116cca0273c416ece9139a78a0/apps/kimi-web/src/style.css) and its in-app design system. This is a historical, inspectable source, not a claim about the current private implementation and not a third-party desktop fork.

The console adopts its cool neutral surfaces, restrained blue accent, compact typography, layered dark mode, consistent radii and progressive disclosure. The Vue/Naive UI implementation is specific to OCG. No Kimi logos, font files, proprietary bundles or chat-specific flows are imported.

The reference's `#1783FF` is not used indiscriminately: light-mode primary action text/fills use the darker `#0967D2`, while dark mode uses an explicit dark foreground on blue buttons. Necessary text is tested on every surface, not only white. The previous seven preference IDs still work; colored themes remain tinted but calmer.

## Implementation map

- `src/theme.ts`: palette, surface semantics, dimensions and public Naive UI theme overrides. `src/styles/main.css`: first-paint fallbacks, focus, type and reduced motion.
- `src/App.vue` / `src/styles/shell.css`: authenticated shell, persisted rail collapse, mobile menu, bounded content and login presentation. Existing session/security handlers and KeepAlive navigation remain.
- `src/components/AppCommandPalette.vue` / `src/domain/navigation-search.ts`: local Ctrl/Command K navigation, label/ID filtering, arrow-key selection and IME-safe shortcut handling. No provider requests or secret access.
- `src/views/Dashboard.vue` / `src/styles/dashboard.css`: labeled connection rows, masked Key controls, smaller decorative mascot, quiet attention list and chart. Existing store calls, explicit rotation confirmation and loading latches remain.
- `AccountCardFrame.vue`: common account presentation and warning-colored cooling state. `FormSurface.vue`: viewport-bounded modal body scrolling; existing validation and save behavior remain in callers.

[`DESIGN.md`](../../DESIGN.md) owns current appearance. Detailed domain interaction rules from the previous design are retained in [`DESIGN.product.md`](../../DESIGN.product.md); its old visual rules are superseded, not its account/routing/Key requirements.

## Regression checklist

Run `pnpm run test:web`, `pnpm run build:web`, and `pnpm run design:lint`. Unit tests cover preference migrations, blocked storage, surface/component agreement, contrast, localized navigation matching, keyboard wrapping and composition-safe shortcuts. These tests do not establish browser or desktop visual correctness.

In a browser, inspect 1440×900, 1280×720, 768×1024 and 390×844 in light/dark mode. Also inspect all colored themes, Chinese/English long labels, non-empty and empty data, loading/retry, long form scrolling, tab/escape/focus restoration, Ctrl/Command K selection and cancellation, collapsed-sidebar persistence and reduced motion. Ensure copying a Key uses the selected credential, rotating it still confirms, and visual inspection does not invoke an upstream probe.

For Tauri, separately check native window resizing, 125%/150% scaling, IME input and clipboard permissions. Record the exact tested commit and distinguish unit, build, browser and desktop results. Screenshots from mocked data must be labeled as fixtures rather than real account balances or service health.

## Component stack direction (Reka UI + Tailwind CSS v4)

New components and new pages prefer **Reka UI primitives styled with Tailwind CSS v4 utilities**; existing Naive UI usage stays and is not proactively rewritten. The rules:

- `src/styles/tailwind.css` is the token bridge: it imports only `tailwindcss/theme.css` and `tailwindcss/utilities.css`, and its `@theme` block maps the runtime `--ocg-*` variables to Tailwind theme keys (`--color-*`, `--font-mono`, `--radius-sm/md/lg`). Values must stay `var()` references so utilities follow all seven runtime themes. Tailwind emits only the theme variables actually used, so unused bridge keys are normal.
- **Never import Tailwind's preflight** — its global margin/padding reset would break Naive UI and the existing stylesheets. Because preflight is off, `<button>`/form elements need explicit `border-0 bg-transparent [font:inherit]`-style utilities.
- Overlay-style components (tooltip, popover, and future menus/dialogs) are wrapped once under `src/components/ocg/` (`OcgTooltip.vue`, `OcgPopover.vue`) and consumed through those wrappers, with a fixed `z-[2000]` alongside Naive UI's dynamically allocated overlay z-indices. Overlay enter/leave uses the shared `.ocg-overlay-*` transition classes (opacity + 2px `translate`, `var(--ocg-motion-fast) var(--ocg-ease)`); `translate` is used instead of `transform` so it never fights floating-ui positioning.
- Reference migration: the Dashboard connection center (tooltips + Key switcher popover) in `src/views/Dashboard.vue`.
