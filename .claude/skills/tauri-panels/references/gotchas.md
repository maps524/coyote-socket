# tauri-panels — gotchas

War stories that will bite you if you don't know them. Svelte 5 + Tauri + CDP.

## Svelte

**This project is Svelte 4.2.7** (`package.json`), even though CLAUDE.md's overview says Svelte 5. Components use `export let` / `$:` / `createEventDispatcher` — legacy Svelte 4 semantics. Don't apply Svelte 5 runes assumptions here.

### `button.onclick` is `null` even though the button works

Svelte's `on:click={fn}` uses `addEventListener`, not the `.onclick` property, so `element.onclick` is always `null` and there's no `__click` own-property to check either. Don't use either to decide whether a handler is attached — attach your own probe listener and dispatch a click instead.

### `element.click()` fires Svelte's handler

Plain `.click()` dispatches a bubbling MouseEvent that Svelte's `addEventListener` handler catches. No need for `Input.dispatchMouseEvent` or synthetic events. `panels click` uses `.click()` internally.

### `svelte-dnd-action` requires an `id` on every item

Each item passed to `dndzone` must carry an `id` property (and the keyed `{#each}` should key by it). Without it the library throws `missing 'id' property for item` as an **async rejection**, which can intermittently abort Svelte's render flush — e.g. it left the preset **reorder modal** (`App.svelte`) stuck open on the X/Escape/overlay close paths while the Done button (a direct parent-state update) still worked. If a dnd modal won't close or updates flicker, check for this rejection in `panels logs <panel> --errors` first. Fixed by tagging working-copy items `{ ...item, id: item.name }`.

### Svelte reactively wipes DOM elements you append directly

If you `document.body.appendChild()` a test element, Svelte's next render cycle may remove it. Debug probes injected via `panels eval` can disappear before the next eval reads them. Prefer storing state on `window.*` globals, which Svelte doesn't touch.

## Tauri / WebView2

### Target IDs rotate on binary swap

Every `build coyote` swaps the underlying executable via the dev-server's shadow copy and relaunches the webview with a fresh CDP target ID. Any caching by target ID breaks after a rebuild. `panels connect` re-resolves targets every invocation. If you write custom CDP code, do the same.

Note: a Rust rebuild relaunches the window, so `panels` state (like the error taps from `panels logs`) is reset — re-run `panels logs <panel>` after a rebuild. A pure frontend edit is HMR-only (Vite stays up, window is NOT relaunched), so state persists.

### Frontend `console.log` does NOT land in coyote-bin stdout

WebView2's console output is isolated from the host Rust process. `dev-server logs coyote-bin` will not show your `console.log`. Use `panels logs <panel>` — it wraps `agent-browser console`, buffers everything with severity prefixes, and (for uncaught exceptions) auto-installs `window.onerror` + `unhandledrejection` taps on first run.

The Rust backend logs separately to `%APPDATA%/com.coyotesocket.app/coyote-socket.log` (and to `dev-server logs coyote-bin`). Frontend and backend logs are two different streams.

### The splashscreen closes fast

`splashscreen` only exists for a moment after launch, then `close_splashscreen` destroys it. Most of the time `panels tabs` will show only `main`. That's expected.

### Invoking backend commands from `panels eval`

CoyoteSocket has `withGlobalTauri` **off**, so `window.__TAURI__` is `undefined`. Invoke backend commands through the internals object instead:

```bash
panels eval main "window.__TAURI_INTERNALS__.invoke('get_presets').then(p => p.length)"   # → 7
```

`agent-browser eval` auto-awaits a returned Promise, so `.then(...)` (or just returning the promise) prints the resolved value. `console.log` inside the eval goes to the page console (read it with `panels logs`), not to stdout.

## Chrome DevTools Protocol

### Screenshots can return stale frames after a state change

`Page.captureScreenshot` sometimes returns a frame from before the latest paint if called immediately after a state-changing click. Fix: double-`requestAnimationFrame` before capture, or just wait ~400ms.

```js
await new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)));
```

### `Runtime.exceptionThrown` is a separate stream from `Runtime.consoleAPICalled`

`agent-browser console` captures `consoleAPICalled` (including `console.error/warn/info/log/debug`) but not bare uncaught exceptions. That's why `panels logs` installs the `window.onerror` + `unhandledrejection` taps — to funnel thrown errors into `console.error` where they get buffered.

## agent-browser CLI

### The first positional to `agent-browser screenshot` is a **selector**, not a path

```bash
# WRONG (silently saves to default tmp dir): agent-browser screenshot foo.png
# RIGHT:
agent-browser screenshot ".some-element" foo.png
agent-browser screenshot                 # no args = viewport capture
```
`panels shot <panel> [sel] [path]` handles this correctly and always prints the final save path. Use the helper.

### Sessions persist across invocations

`agent-browser` caches a "default" session keyed by the CDP connection. If a previous call bound to another port (e.g. personal Edge on 9222), a later `--cdp 9224` does NOT override — it uses the cached session. Fix: `agent-browser close --all`. `panels connect` does this reset for you.

### Windows `.cmd` shim flashes a console window

`agent-browser` installs as a `.cmd` shim. Invoking it spawns cmd.exe, which flashes a console window and destroys argument quoting for `agent-browser eval`. `panels cli.mjs` resolves the underlying `.exe` and spawns it directly with `shell:false, windowsHide:true`. Don't re-invent this — use `panels`.
