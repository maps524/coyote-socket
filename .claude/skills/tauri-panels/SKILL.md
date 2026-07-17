---
name: tauri-panels
description: "See, screenshot, automate, and debug the running CoyoteSocket Tauri WebView (the main app window + splashscreen). Use whenever UI or design work would benefit from looking at the actual running interface — iterating on a Svelte component, capturing an element for feedback, verifying a visual change landed, tailing frontend console/errors, clicking buttons or into modals/reorderable lists, or automating the UI. Triggers: 'look at the app', 'see the UI', 'screenshot the window', 'how does X look', 'give me feedback on the component', 'inspect the app', 'verify my Svelte change', 'debug webview', 'frontend logs', 'catch the exception', 'browser console', 'click the button', 'open the reorder modal'."
---

# tauri-panels

See and interact with the running CoyoteSocket Tauri WebView. **Use this any time the task touches UI, design, or frontend behavior.** Look before you edit; tail logs before you guess.

Driver: `node .claude/skills/tauri-panels/cli.mjs <cmd>` — referred to as `panels` below. Always prefer this over raw `agent-browser`; it handles the CDP port, Svelte selectors, window naming, Windows spawning, tag-scroll-capture, and frontend exception taps for you.

## Commands

```bash
panels connect                                      # One-time: reset + verify CDP + list open windows
panels tabs                                         # Show currently-open windows with names & sizes
panels shot <panel> [sel] [path]                    # Screenshot window or element in it
panels full <panel> [path]                          # Full-page screenshot
panels logs <panel> [--errors] [--clear]            # Console buffer + uncaught exceptions
panels click <panel> <child-sel> [--in <c>] [--where <text>]
                                                    # Click nested element in text-matched container
panels styles <panel> <selector>                    # Computed styles + box for an element
panels eval <panel> "<js>"                          # Run JS in a window
panels reload [panel] [--wait-for <sel>] [--timeout <ms>]
                                                    # Hard reload, optionally wait for mount
panels help                                         # Full reference
```

**Panels**: `main | splashscreen` — pass by name, not index. Resolution is by URL (+ window size), so it survives binary swaps and tab reorders. `main` is the app; `splashscreen` only exists briefly at launch.

**Screenshots** default to `.claude/tmp/panels/<panel>-<ts>.png` (gitignored). Pass a path to override.

## Canonical flows

### Review a UI change
```bash
panels shot main                              # capture
# → Read the PNG with the Read tool
# → Edit the Svelte source
# → HMR reloads automatically (Vite stays up across Rust rebuilds)
panels shot main                              # capture again, compare
```

### Screenshot one element
```bash
panels shot main ".preset-select"             # scoped to the element (auto scroll-into-view)
```

### Catch a frontend bug
```bash
panels logs main --clear                      # fresh buffer
# ... reproduce the bug in the UI ...
panels logs main --errors                     # [error], [warning], [UNCAUGHT], [REJECT]
```
`panels logs` installs `window.onerror` + `unhandledrejection` taps on first use, so uncaught exceptions route through `console.error` and land in the same buffer. (WebView2 console output does NOT reach `dev-server logs coyote-bin` — use this instead.)

### Click something nested in a modal / reorderable list
```bash
panels click main "button" --in "[role=dialog]" --where "Reorder"
# "click the button inside the [role=dialog] whose text contains 'Reorder'"
```
Container + `--where` matches by case-insensitive substring of `textContent`. Use when the list reorders and positional selectors don't work.

### Read a computed style
```bash
panels styles main ".preset-select"
```

### One-off JS
```bash
panels eval main "document.querySelectorAll('[role=dialog]').length"
# withGlobalTauri is off — invoke backend commands via the internals object:
panels eval main "window.__TAURI_INTERNALS__.invoke('get_presets').then(p => p.length)"
```

## Selector notes

- Plain `.foo` auto-rewrites to `[class*=foo]` — survives Svelte's scoped class hashes.
- Use `[attr=value]`, `#id`, or `[data-testid=...]` for exact matches.

## Preconditions

`panels connect` verifies CDP is up and lists the open windows. If the app isn't running with remote debugging, see `references/wiring.md`.

Common gotchas (Svelte 5, Tauri, CDP): see `references/gotchas.md`.

## Raw agent-browser

For anything this skill doesn't cover (`agent-browser stream enable` for a live WebSocket feed, interactive AI chat mode, network routing, keyboard events, etc.), run `agent-browser --help`. **Don't use raw `agent-browser` for this app** — you'll rediscover all the project-specific gotchas this helper already handles.
