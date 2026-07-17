# tauri-panels — how CDP is wired

Only read this if `panels connect` reports CDP is unreachable, or you're changing the wiring.

## The chain

1. **Dev-server daemon** (`.claude/skills/dev-server/`) launches `coyote-bin` with the env var `COYOTE_REMOTE_DEBUG_PORT=9224` (set in `services/coyote-bin.mjs`).
2. **`fn main()`** in `src-tauri/src/main.rs` — at the very top, before `tauri::Builder::default()`, reads that env var and sets `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=${port}`.
3. **WebView2** reads that env var at init and opens a Chrome DevTools Protocol server on the port.
4. **agent-browser (via `panels` CLI)** connects to `http://localhost:9224/` and enumerates targets.

## Why an env var instead of `#[cfg(debug_assertions)]`

Dev runs under the dev-server skill use a `release-fast` build (see `services/coyote-bin.mjs`), so `debug_assertions` is off. Env-var gating works in any build mode and keeps CDP off by default in production — `npm run tauri:build` never sets `COYOTE_REMOTE_DEBUG_PORT`, so shipped binaries have CDP disabled.

## Why 9224

- `9222` — conventional CDP port, but the maintainer's personal Edge/Chrome may use it. Attaching there could enumerate personal tabs instead of the app.
- `9223` — used by the ai-notifications project's `tauri-panels` (its notification server is always running).
- `9224` — free, and distinct from both. Also distinct from the dev-server daemon port (`9860`).

## Reloading the daemon config after changing the port/env

The dev-server daemon loads plugin files from `services/` at startup and on `POST /reload-services`. After editing `services/coyote-bin.mjs`:

```bash
# Re-read plugins, then restart coyote-bin so the new env takes effect
node .claude/skills/dev-server/cli.mjs restart coyote-bin
```

If you changed the Rust side (`main.rs`), rebuild the binary:

```bash
node .claude/skills/dev-server/cli.mjs build coyote
```

## Quick diagnostics

```bash
# Is the app binary up?
node .claude/skills/dev-server/cli.mjs status

# Is CDP actually listening?
curl -s http://localhost:9224/json/version
# Expected: {"Browser":"Edg/...", ...}

# Raw target list (bypasses agent-browser)
curl -s http://localhost:9224/json
```

If `curl /json/version` returns a browser identity but `panels connect` still fails, the issue is in the helper script or agent-browser's cached session — try `agent-browser close --all` then retry.

## The Windows spawn gotcha (for anyone touching cli.mjs)

The helper resolves `agent-browser` to the real `.exe` underneath the npm `.cmd` shim, then spawns it directly with `shell: false` and `windowsHide: true`. This avoids:

- Console window flash on every invocation (from the `.cmd` shim spawning cmd.exe).
- The Windows cmd.exe quoting nightmare when passing JS code to `agent-browser eval`.
- Node's `spawn` not finding `.cmd` files without `shell: true`.

The real path is discovered by running `where agent-browser`, reading the `.cmd` file, and extracting the `.exe` path from the line that wraps it. If the upstream npm package reorganizes its binary layout, this resolution will need updating — see `resolveAbBin()` in `cli.mjs`.
