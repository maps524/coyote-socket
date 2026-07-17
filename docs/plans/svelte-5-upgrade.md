# Full-Bore Frontend Upgrade — Svelte 5 + Vite 8 + Tailwind 4

**Scope (decided):** go all in — Svelte 4→5 **with full runes migration**, Vite 5→8, Tailwind 3→4.
**Branch:** `svelte-5-upgrade`. **Status:** PREP only — `package.json` bumped + this doc. Nothing installed, migrated, built, or committed.

**Base (confirmed):** `worktree-pipeline-refactor` @ **`f342e0a`** — **not** `main`. Jessica signalled done (2026-07-17); her gamepad/delete/tauri-panels work is committed:
- `f342e0a` feat(presets): delete + gamepad jump-to-preset in reorder modal, fix close; adds `GamepadBindControl.svelte`; Popover fix
- `e49a6f4` chore(dev): tauri-panels CDP skill + WebView2 remote-debug wiring

So `App.svelte`, `Popover.svelte`, and the new `GamepadBindControl.svelte` already carry that work. Apply the two prepped artifacts (`package.json` bumps + this doc) on top.

**⚠️ COORDINATION BLOCKER — do not start Phase 1 until resolved.** The primary worktree still has **uncommitted transforms-pipeline WIP that touches the frontend**, not just Rust:
- `src/lib/components/ui/TransformsEditor.svelte` (modified)
- `src/lib/components/ui/ScalarInputControl.svelte` (new, untracked)
- `src/lib/types/modulation.ts` (modified)
- plus Rust: `modulation/processing/resolver/settings_convert/transforms/*`

A full-tree migration (`@tailwindcss/upgrade` rewrites utility classes across **all** `.svelte`; `sv migrate` rewrites runes across all components) would collide with and churn this uncommitted work. **Resolve first — one of:** (a) that work gets committed/stashed so the tree is clean, or (b) run the migration in an isolated worktree off `f342e0a` (needs separate dev-server/CDP ports to coexist with the running primary). Confirm the approach with the maintainer before installing anything.

---

## Strategy

Svelte 5 runs Svelte 4 code in **legacy mode**, and both Vite 8 and Tailwind 4 ship official codemods. So even "full bore" is: **land the dependency majors green first (code still legacy), then migrate to runes** — never rewrite components with no compiler in the loop.

Two rules:
1. **Compiler-green before rewrites.** Get `svelte-check` + `vite build` + the running app green on the new deps *in legacy mode* before touching a single component for runes. This separates "do the new majors run our code" from "did we migrate correctly."
2. **Bisectable commits.** Each dependency major and each migration wave is its own commit, so a regression bisects to one change. End state is identical; the path is just safe.

Phases: **1** = dependency lift to green (3 sub-steps), **2** = full runes migration (waves + sub-agents), **3** = verify. Each phase independently shippable.

Run everything below in the **primary worktree** on top of Jessica's committed gamepad work (so `node_modules`, dev-server, and `tauri-panels` are available). See Base & handoff above.

---

## Dependency matrix (staged in `package.json`)

| Package | From | To | Notes |
|---|---|---|---|
| `svelte` | ^4.2.7 | **^5.56.6** | legacy mode compiles existing code; runes in Phase 2 |
| `@sveltejs/vite-plugin-svelte` | ^3.0.0 | **^7.2.0** | peer: `svelte ^5.46`, `vite ^8` — forces the Vite 8 bump |
| `vite` | ^5.0.0 | **^8.1.5** | 3 majors; plugin 7 requires it |
| `svelte-check` | ^3.6.0 | **^4.7.3** | Svelte 5 checker |
| `lucide-svelte` | ^0.294.0 | **^1.0.1** | ⚠️ 0.294 peer `<5`; hard blocker + biggest icon-rename churn |
| `svelte-dnd-action` | ^0.9.69 | **^0.9.74** | already Svelte-5-ready |
| `tailwindcss` | ^3.4.0 | **^4.3.3** | CSS-first rewrite; use `@tailwindcss/vite` |
| `@tailwindcss/vite` | — | **^4.3.3** (new) | replaces the PostCSS Tailwind plugin |
| `tailwind-merge` | ^2.0.0 | **^3.6.0** | v3 understands Tailwind 4 class groups (used by `cn.ts`) |
| `tailwind-variants` | ^0.1.18 | **^3.2.2** | ⚠️ appears **unused** (only in `cn.ts`'s dir, not imported) — consider dropping instead |
| `@tsconfig/svelte` | ^5.0.0 | **^5.0.8** | minor |
| `autoprefixer` | ^10.4.16 | **removed** | Tailwind 4 handles prefixing |
| `postcss` | ^8.4.32 | **removed** | `@tailwindcss/vite` bypasses PostCSS; Vite bundles its own |

---

## Phase 1 — dependency lift → green (legacy mode)

Do the three majors as separate commits. After each: `npm run check` (warnings OK, no errors), `vite build`, launch via dev-server, `panels connect`.

### 1a. Svelte 5 (legacy) + lucide
- [ ] `npm install`
- [ ] Fix `lucide-svelte` 0.294→1.0 breakage. Import path unchanged (`import { X } from 'lucide-svelte'`) but some icon names changed. Audit every icon import — `ui/GamepadIcon.svelte` (~20), plus `App.svelte`, the pills, panels — against the v1 export set.
- [ ] `npm run check` — legacy mode should pass with **deprecation warnings** (slots, `createEventDispatcher`, `beforeUpdate`/`afterUpdate`); leave warnings for Phase 2.
- [ ] Add a minimal `svelte.config.js` only if the checker/plugin demands one.
- [ ] Commit: `chore(deps): svelte 5 (legacy mode)`

### 1b. Vite 8 + plugin 7
- [ ] `@sveltejs/vite-plugin-svelte@7` + `vite@8` (already in package.json).
- [ ] `vite.config.ts` review: current config is simple (`svelte()` plugin, `$lib` alias, Tauri server block). Vite 8 needs **Node 20+**; confirm the environment. Watch for: `defineConfig(async …)` still fine, plugin option shape, default build target bump.
- [ ] Confirm dev-server skill still serves `:1421` and the `DEV_URL` shadow-swap still loads (no contract change expected).
- [ ] `vite build` + run. Commit: `chore(deps): vite 8`

### 1c. Tailwind 4
- [ ] Run the official codemod: `npx @tailwindcss/upgrade`. It converts `@tailwind` directives → `@import "tailwindcss"`, migrates `tailwind.config.js` theme → CSS `@theme`, rewrites renamed utility classes across all `.svelte`/`.html`, and updates deps.
- [ ] Wire the Vite plugin: add `@tailwindcss/vite` to `vite.config.ts` plugins; **delete `postcss.config.js`** and `tailwind.config.js` (or keep the config via `@config` if the codemod chose that).
- [ ] `src/app.css` specifics: `@tailwind base/components/utilities` → `@import "tailwindcss";`; the `@layer base { @apply border-border; bg-background … }` and `@layer utilities` blocks — verify the codemod's handling (custom utilities may become `@utility`). The HSL CSS-var colors (`--border`, `--primary`, …) move into `@theme` as `--color-*`.
- [ ] Manual utility audit (codemod covers most, but verify visually): default **border color** changed (gray→currentColor), `shadow-sm`→`shadow-xs` / `shadow`→`shadow-sm`, `outline-none`→`outline-hidden`, `ring` default 3px→1px (add `ring-3` where the old width mattered — e.g. focus rings in `Dialog`, `Button`). `cn.ts` + `tailwind-merge@3` handle runtime class merging.
- [ ] Screenshot-compare key screens before/after via `tauri-panels` (theme, borders, focus rings, shadows are the usual regressions).
- [ ] Commit: `chore(deps): tailwind 4`

**End of Phase 1: fully-upgraded deps, app green and visually intact, code still legacy Svelte.** Shippable checkpoint.

---

## Phase 2 — full runes migration

Only after Phase 1 is green + committed.

- [ ] Run `npx sv migrate svelte-5` (official codemod): `export let`→`$props()`, `$:`→`$derived`/`$effect`, `createEventDispatcher`→callback props, `<slot>`→`{@render}`/snippets, lifecycle shims; inserts `@migration-task` markers where unsure.
- [ ] Review the whole codemod diff. Commit: `refactor: apply svelte-5 codemod`.
- [ ] `npm run check` → collect `@migration-task` sites + errors.
- [ ] Assign components to sub-agents by wave. **Each sub-agent MUST run `npx svelte-check --tsconfig ./tsconfig.json` and report 0 errors for its file before checking its box.** No blind edits; migrate a child before its parent.

### Wave A — leaf UI (do first)
- [ ] `ui/Button.svelte` (slot, `$$restProps`)
- [ ] `ui/Input.svelte` (`$$restProps`)
- [ ] `ui/Select.svelte` (slot, `$$restProps`)
- [ ] `ui/Toggle.svelte` (`createEventDispatcher`)
- [ ] `ui/Slider.svelte` (`createEventDispatcher`)
- [ ] `ui/StatusIndicator.svelte`
- [ ] `ui/GamepadIcon.svelte`
- [ ] `ui/Tooltip.svelte` (slot)

### Wave B — composite UI (consume Wave A)
- [ ] `ui/Dialog.svelte` (`createEventDispatcher` close, slot) — modals depend on it
- [ ] `ui/Popover.svelte` (`createEventDispatcher`, named `trigger` slot) — delete/gamepad-bind popovers depend on it
- [ ] `ui/Tabs.svelte` / `ui/TabsClassic.svelte` (`createEventDispatcher`, slot)
- [ ] `ui/PortInput.svelte` (`createEventDispatcher`)
- [ ] `ui/RangeSliderWithIndicator.svelte` (`createEventDispatcher`)
- [ ] `ui/GamepadBindControl.svelte` (`createEventDispatcher`: start/save/cancel/clear; consumes Button + GamepadIcon; used by App.svelte reorder rows + settings rebind list)
- [ ] `ui/TransformsEditor.svelte` (`createEventDispatcher`) — ⚠️ has uncommitted transforms-WIP edits; reconcile before migrating
- [ ] `ui/ScalarInputControl.svelte` — ⚠️ new from transforms WIP; add once committed

### Wave C — feature panels/pills (mostly plain `export let`)
- [ ] `ChannelControl.svelte`
- [ ] `ConnectionPanel.svelte`
- [ ] `BluetoothPanel.svelte`
- [ ] `InputMonitor.svelte`
- [ ] `LogsPanel.svelte`
- [ ] `WaveformChart.svelte` / `SynthWaveformChart.svelte`
- [ ] `InputStatusPill.svelte` / `OutputStatusPill.svelte` / `GamepadStatusPill.svelte`
- [ ] `settings/GeneralTab.svelte`
- [ ] `settings/ButtplugTab.svelte`

### Wave D — root (last)
- [ ] `App.svelte` — largest; consumes everything, holds the input-action dispatch, reorder modal, delete + gamepad-bind. Migrate after all children so the callback-prop/snippet contracts are settled.

### Runes cheatsheet
- `export let x` → `let { x } = $props()`; default `= d`; two-way → `$bindable()`.
- `$: y = f(x)` → `const y = $derived(f(x))`; `$:` side-effect block → `$effect(() => {…})`.
- `createEventDispatcher()` + `dispatch('close', d)` → callback prop `let { onclose } = $props()` + `onclose?.(d)`; update the parent's `on:close={…}` → `onclose={…}` in the same commit.
- `<slot/>` → `let { children } = $props()` + `{@render children?.()}`; named/`trigger` slot → snippet props (`{#snippet trigger()}` at call site).
- `$$restProps` → `let { ...rest } = $props()` + `{...rest}`; `$$slots` → check the snippet prop for existence.
- Stores (`$store`) unchanged — keep as-is; don't convert to runes.
- `svelte-dnd-action`: item `id` requirement unchanged (already satisfied by the reorder working-copy `{...preset, id}`).

---

## Phase 3 — verification (tauri-panels)

- [ ] Main screen renders; channel A/B sliders, selects, buttons.
- [ ] Preset dropdown + reorder modal: drag reorder persists, delete pop-confirm, gamepad-bind popover, X/Escape/overlay close.
- [ ] Settings dialog: tabs, gamepad rebind list, close paths.
- [ ] Bluetooth / connection panels; Logs panel.
- [ ] Tailwind visual pass: borders, focus rings, shadows, dark theme intact.
- [ ] `panels logs main --errors` clean across all screens.
- [ ] `npm run check` 0 errors, 0 (or intentional-only) warnings.

---

## Risk register
- **lucide-svelte 0.294→1.0** — icon renames; largest Phase-1 code touch. Mitigate: grep all imports, reconcile against v1 exports.
- **Tailwind 4 visual regressions** — border color / shadow / ring defaults changed. Mitigate: before/after screenshots per screen.
- **Vite 8 / Node** — needs Node 20+; verify runtime.
- **Codemod residue** — `@migration-task` markers need human judgment; never auto-accept.
- **`App.svelte` size** — do it last, its own commit, with a full panels smoke test after.
