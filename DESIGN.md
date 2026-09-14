---
name: Open Console Gateway
colors:
  canvas: "#F7F7F8"
  surface: "#FFFFFF"
  ink: "#18181B"
  muted: "#5F6068"
  primary: "#18181B"
  primary-soft: "#ECECEF"
  success: "#0B6844"
  warning: "#8A4D00"
  error: "#A92742"
  info: "#245DB6"
typography:
  display: "Bahnschrift, Segoe UI Variable Display, sans-serif"
  body: "Segoe UI Variable Text, Noto Sans SC, Microsoft YaHei UI, sans-serif"
  data: "Cascadia Mono, Consolas, monospace"
spacing:
  xs: 4
  sm: 8
  md: 12
  lg: 16
  xl: 24
  xxl: 32
rounded:
  small: 6
  medium: 10
  large: 14
---

# Open Console Gateway

## Overview

Open Console Gateway is a local multi-account operations console. Its signature is a first-screen connection center paired with the OpenCode mascot. The interface is compact, technical, calm, and unmistakably operational rather than promotional.

## Colors

The selector contains seven two-character themes: 默认, 皓白, 曜黑, 藤紫, 霁蓝, 青瓷, and 暖铜. 默认 follows the operating system and resolves to 皓白 or 曜黑; every other theme is fixed. 皓白 stays neutral and 曜黑 uses a pure-black canvas.

Frontmatter colors above are the **皓白** baseline (also the CSS custom-property defaults before JS applies a theme). Runtime tokens live in `src/theme.ts` (`THEME_TOKENS`); light themes share the same success / warning / error / info values for WCAG AA contrast. 曜黑 substitutes its own dark-mode semantic set. Chart series beyond the theme primary use `CHART_PALETTE` in `src/theme.ts` and may keep brighter fixed hues than the status colors.

The four colored themes tint the full environment—canvas, surfaces, raised controls, borders, and interaction states—so they must never collapse into white cards with isolated accent colors.

Use each theme's primary color for active navigation, focus, primary actions, and the first chart series. Use success only for successful or available states; semantic status colors never change meaning between themes.

## Typography

Headings use `{typography.display}`. Interface copy uses `{typography.body}`. API addresses, keys, costs, and other machine-readable values use `{typography.data}` with tabular numerals.

The type scale has six steps, exposed as `--ocg-font-xs` … `--ocg-font-2xl`: 12px for captions and field labels, 13px for secondary text, 14px as the body base, 16px for card titles, 20px for KPI figures and page titles, and 24px reserved for the connection hero. Hierarchy comes from this scale combined with weight and color; never introduce ad-hoc sizes outside the six steps.

## Layout

Use the spacing scale from `{spacing.xs}` through `{spacing.xxl}`. The side rail (horizontal app menu below 1024px) exposes seven fixed core views in this order: Dashboard, Access Keys, Accounts, Providers, Aliases, Logs, Settings. A divider below Settings starts the optional **Extensions** group, with CPA as its local-only entry. CPA's runtime boundary remains a static local-service integration rather than a Provider, Plan, or dynamic plugin. The CPA page tabs are Overview, Accounts, Model catalog, and (managed runtime only) Runtime logs. Model catalog shows the saved snapshot as selectable cards grouped by CPA-reported source, about three to four per row; a highlighted card joins routing. Refresh is explicit; newly discovered IDs stay off until selected. Managed-runtime client keys stay on Overview because OCG routing uses the protected Inference Key automatically.

The Dashboard order is connection center, KPIs, needs-attention list, then the full-width daily Token chart. Core connection information must stay above the fold and must never be moved into a secondary rail. The connection center is the consume surface: the current Key, copy, and rotate-current stay there, plus a manage action that opens Access Keys. Create, rename, enable, delete, and reset live only on Access Keys. The primary key has no custom-value field; rotation uses the same reset control as sub keys.

Providers is the supplier control plane. The left rail lists the V4 connection projection (`GET /dashboard/api/v4/connections`): built-in Providers with at least one account, every user-defined Provider (with or without a Key), and each Custom API account as its own row, grouped by Plan/API. Unused built-in templates stay off the rail and remain available only in the Add catalog. User-defined Providers are not a separate navigation item. Row and detail-header status comes from server-side projection fields only: **Missing credential** (authorization `missing`), **Disabled** (lifecycle `disabled` or all credentials disabled), **Invalid credential** (authorization `invalid`), **No enabled model** (eligibility reason `no_enabled_target`), **Cooling down** (eligibility `cooling`). These are local eligibility projections, never upstream health; `unknown` authorization shows no badge and is not verified. The rail footer's single **Add Provider** action opens a preset browser in the main pane (Plan/API groups, vendor families, search, and a manual entry); picking a preset or manual setup switches the main pane to the embedded creation form. That form can **Save draft** once name and URL are valid (Key and models may be omitted) or **Complete setup** (models required, and a Key for keyed auth). Drafts persist on the V4 connection and appear in the Providers rail with a **Draft** status and **Continue setup**; they are not routed and must not open the configured **Add Key** flow merely because a credential is missing. Reopening a draft keeps the same connection/provider ID, shows a saved-Key indicator from V4 rather than plaintext, and sends `mode=draft` or `mode=complete` on the same ID. Completing with a saved Key exposes an explicit current-address authorization checkbox (unchecked on a new or changed address; opening or editing never grants). Creating a user-defined Provider — from **Providers → Add Provider** or **Accounts → Add account** → preset — commits through `POST /dashboard/api/v4/onboarding/commit`. Configured Providers still use ordinary V3 edit. **Add Key** on a configured Provider opens the same credential editor used on Accounts, prefilled for that Provider. Provider-preset and user-defined rows share one detail shell: a header with the brand mark, name, origin badge (供应商预设 / 自定义), and edit/delete where allowed, followed by up to three tabs — Models (model catalog and protocol matrix, or read-only mappings for user-defined rows), Pricing (only when priced), and Settings (connection facts; OpenCode Go keeps the managed-signup invite URL here). User-defined Providers bind Configurable HTTP, stay unpriced, and expose create/edit/delete plus optional discovery/test. Accounts of those Providers are Key-only and do not own Endpoint, protocol, or model mappings. A Custom API account row shows a read-only summary (endpoint, protocol, mapped models) with **Edit on Accounts** opening the existing account editor; Custom API configuration remains account-owned and is edited only on Accounts.

Aliases is a separate core inspection page because it aggregates mappings across every currently enabled account rather than belonging to one selected Provider. It shows only those Providers — including CPA as its own Provider — that have at least one enabled account. Each row is a client-facing name, Provider, and exact upstream identity. CPA rows come from the selected catalog snapshot. Flag overlapping public names and upstream IDs for inspection. Search spans names, upstream IDs, and Providers. It is never an Alias editor, API, store, or cache. Custom mappings are edited only on Accounts.

Every provider scope uses one Model catalog composition: a compact source line and, when the scope has an official catalog, the same **Refresh model catalog** action sit in the panel header; the per-model list follows directly without a separate catalog-summary card or account picker. OpenCode Go and Zen Free refresh from their official sources, with the backend selecting any required eligible credential. A persisted official `/models` snapshot is authoritative; the catalog stays empty until that first successful refresh. Models newly added by a refresh appear with their enable switch off until the user explicitly turns it on. Existing model overrides and probe state survive refreshes. The list has one row per model with columns: model (alias plus raw upstream ID), upstream protocol, enable, and a row action. Available upstreams always use the same chips: a visible chip can connect; blue is the conversion default for unmatched clients including Gemini. Clicking a chip sets that default. MiniMax CN and Kimi Code CN start with Chat Completions and Messages chips; neither advertises Responses. Enabling a model force-enables every available protocol and does not disable siblings. The enable switch flips the model's effective on/off state for routing; an enabled switch uses the success color while a disabled one stays neutral, never error red. Scope-level batch actions stay out of the idle toolbar: **Select** enters multi-select, adds a leading checkbox column, and the toolbar trailing slot becomes **On**, **Off**, and **Delete** for the checked rows. Each row ends with a compact Test icon (when probing is supported) and a Delete icon. Delete removes the model from the persisted local catalog snapshot so it stops routing; an official catalog refresh may add that ID back, off by default. The Test action probes only that model's effective preferred protocol through backend-managed eligible-account fallback and never flips switches. No account picker is shown. OpenCode Go and Zen Free use their constructable sets, GOAT probes its sealed native family path, and MiniMax CN plus Kimi Code CN probe their sealed Chat Completions and Messages paths. Command Code GOAT is live and refreshes from its official public catalog; GOAT preset rows default on and additional discovered rows default off until an explicit switch enables their supported protocol. Prefer monospace for revision IDs, model IDs, USD rates, and multipliers. Catalog fetch, protocol probes, and OpenCode Go pricing refresh are explicit primary actions, never automatic on page load. The Provider Test action must warn that real minimal requests may be sent through multiple eligible accounts and may consume quota. Accounts keep identity, Key, enablement, usage, and every Custom mapping. Ready account cards use a success tag for **Enabled** and an error tag for **Disabled** so the routing gate is visible; cooling stays warning and auth failure stays error. This is distinct from the Providers model enable switch, whose off state stays neutral. Every ready account card has the same compact Test connection utility action. It opens a searchable model table and locks every single or sequential batch test to that exact account with no fallback; test results remain local to the open dialog. Lifecycle-bearing cards expose their expiry tag as a compact purchase-date editor with a date picker and a set-to-today action; Zen Free and Custom API omit expiry UI. Kimi and MiniMax account cards keep a neutral not-yet-refreshed quota bar before their first official usage snapshot, then replace it with the returned windows. The Custom account form exposes one account-wide protocol selector and a compact mapping table of public name to upstream ID. Discovery lists upstream IDs only; importing one creates an exact public-name = upstream-ID row and never strips or synthesizes a suffix. Add Account is a grouped plan list with a detail pane for copy and actions, not a two-column card grid. Backend-owned singletons such as Zen Free are omitted from that list and enabled from the account list.

## Shapes

Controls use `{rounded.small}` or `{rounded.medium}`. Content panels use `{rounded.large}`. Avoid excessive pills and ornamental cards.

## Components

Utility actions are circular quaternary icon buttons with a Tooltip and an explicit accessible name. Primary commit actions and destructive confirmations retain visible text. Connection rows combine one semantic icon, one monospace value, and only the actions needed for that value.

Provider rows in the chooser and rail use a brand mark: the vendor's brand SVG when a CC0 asset exists under `src/assets/provider-logos/`, otherwise a tinted monogram block carrying the vendor's initial. Brand marks are decorative; the adjacent text label always carries the vendor name.

## Do's & Don'ts

- Do call the access credential “Key”; never display “Gateway Key”.
- Do keep API, Key, and upstream copy actions adjacent to their values.
- Do use icons to reduce repeated labels, while retaining screen-reader labels.
- Do preserve visible keyboard focus and reduced-motion preferences.
- Do keep theme names to two Chinese characters and expose all seven choices in one selector.
- Do give the mascot a subtle light rim only in 曜黑; other themes use the normal shadow.
- Don't reuse the success green as a brand primary color.
- Don't repeat a card title when structure and icons already provide context.
- Don't hide primary connection actions behind menus or secondary navigation.
- Don't use icon-only controls for ambiguous commit or irreversible actions.
- Don't fetch OpenCode Go pricing, provider catalogs, protocol probes, or GitHub releases without an explicit user action.

## Responsive

At widths below 1024px, replace the sidebar with the horizontal application menu while retaining the Settings divider and Extensions group. On narrow phones, connection rows remain full width and the mascot becomes a low-opacity background element that cannot cover controls.

## Iteration Guide

Before adding visible copy, ask whether an icon, value, structure, or Tooltip already communicates it. Before adding a component or dependency, reuse Naive UI and the existing native platform capability. When changing colors or type scale, update `src/theme.ts` and this file together, then run `pnpm run design:lint` and `src/theme.test.ts`.

## Account and Provider workflow refinements

Provider management defaults to built-in and saved connections; creation templates belong to the New Provider flow. Add Account and Add Provider can both save an onboarding draft; continuing that draft lives on Providers. Add Account distinguishes existing connections from adding a new service, preserving Plan/API and vendor grouping within creation. A compact variant picker must leave the Key form usable at 1280×720 and 390×844. Show a read-only endpoint/protocol summary before credential entry. Preserve known preset branding on saved connections, with contrast-safe monochrome assets in dark themes. Model tables have their own search/filter and clearly named connection tests; public and upstream identities remain reachable on phones.

Enabled account/model states describe configuration, not a successful upstream test. Dynamic Providers have no inferred subscription expiry. CN model protocol choice persists independently of its enable switch. Platform Add Key creates and associates one exact account; partial association failure exposes a retry of association only, never a duplicate creation. Existing-Key linking remains available.
