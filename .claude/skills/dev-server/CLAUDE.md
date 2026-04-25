# Dev-Server Skill — Notes for future agents

## Plugin architecture

Every service, build target, and clean target lives in its own **plugin file** at `services/{name}.mjs`. Each plugin exports a factory function `(ctx) => ({ ... })` that receives shared context (paths, env) and returns a descriptor object.

No hardcoded service definitions exist in `daemon.mjs` — the daemon loads all plugins from `services/` at startup and on `POST /reload-services`.

## Adding a new service

1. **Create `services/{name}.mjs`** — copy an existing plugin as a template (`vite.mjs` for a non-buildable Node process, `coyote-bin.mjs` for a Rust binary with shadow-copy + build target, `tauri-dev.mjs` for a long-running watcher).

2. **Pick hotkeys** — set `dashboard.logFilterKey` (digit) and `dashboard.buildKey` (letter) in the plugin. The daemon validates uniqueness on load — duplicates throw immediately. Currently used keys: log filters `1` (vite), `2` (coyote-bin), `3` (build, hardcoded cross-cutting). Build keys: `b` (coyote-bin). Destructive global actions (`S`=stop, `K`=kill, `R`=restart-all) must be Shift-required.

3. **Reload** — no daemon restart needed. Run `POST /reload-services` (or press `r` in the TUI dashboard). The daemon re-imports all plugin files, diffs against the live state, adds/removes/restarts services as needed.

4. **Update orchestration** — if the new service should start automatically with `dev`, add it to `orchestrateDev` in `daemon.mjs`. This is the one cross-cutting workflow that still lives in `daemon.mjs`.

5. **Update help/docs** — add to `cli.mjs` help text (`SERVICES:`, `BUILD TARGETS:`, `CLEAN TARGETS:` sections), `SKILL.md` tables, and `ALIASES` map for CLI shorthand.

## Plugin file shape

```js
export default (ctx) => ({
  name: "xxx",
  service: {                         // omit if not a running process
    binary: ctx.bin("xxx"),          // OR command: "npx", args: [...]
    shadowCopy: true,                // run from .active.exe to avoid file locks
    args: [],
    env: { DEV_URL: ctx.VITE_DEV_URL },
    requires: ["vite"],              // auto-start deps, watchdog restarts
    group: "tauri",                  // optional: mutually exclusive group
    healthUrl: "http://...",         // optional: poll for 2xx after start
    noTreeKill: true,                // optional: kill only the process, not children
  },
  build: {                           // omit if not buildable
    command: "cargo",
    args: ["build", "--release"],
    cwd: ctx.SRC_TAURI_DIR,          // optional: defaults to PROJECT_ROOT
    service: "xxx",                  // auto-restart this service after successful build
    aliases: ["x"],                  // additional names registered in BUILD_DEFS
  },
  clean: {                           // omit if not cleanable
    packages: ["xxx"],
    cwd: ctx.SRC_TAURI_DIR,          // optional: defaults to PROJECT_ROOT
    service: "xxx",                  // stop/restart around clean
    safe: true,                      // false = requires --force
    aliases: ["x"],
  },
  dashboard: {
    short: "xxx",                    // 5-char log-line label
    color: "cyn",                    // ANSI color key (ylw/cyn/blu/grn/mag/red)
    order: 25,                       // sort position in service list
    logFilterKey: "9",               // digit for log filter toggle
    buildKey: "x",                   // letter for build hotkey
  },
});
```

## ctx object

The factory receives a context with these fields:
- `IS_WIN` — boolean, platform check
- `RELEASE_DIR` — `src-tauri/target/release/`
- `PROJECT_ROOT` — repo root
- `SRC_TAURI_DIR` — `src-tauri/` (use as `cwd` for `cargo` commands)
- `VITE_DEV_URL` — `http://localhost:1421` (override with `COYOTE_VITE_URL`)
- `bin(name)` — resolve a binary path under `RELEASE_DIR` with the platform suffix (`.exe` on Windows)

## Daemon restart policy

`POST /reload-services` handles plugin changes without bouncing the daemon. Only changes to `daemon.mjs` itself (HTTP routes, orchestration logic, crash-loop tuning) require a full daemon restart (`shutdown` then any CLI command auto-starts a fresh one). Never restart without explicit user go-ahead — it kills all child processes.

## Reserved TUI keys (non-plugin, hardcoded)

| Key | Action |
|-----|--------|
| `3` | Toggle build log filter |
| `a` | Show all logs |
| `r` | Reload plugins (POST /reload-services) |
| `R` | Restart all running services (Shift — disruptive) |
| `S` | Stop all (Shift — destructive) |
| `c` | Clean sub-menu |
| `d` | `dev` orchestration |
| `q` | Quit dashboard |
| `K` | Kill daemon (Shift — destructive) |

## Differences from the ai-notifications source

This skill was ported from `C:/Dev/Repos/explore/tauri/ai-notifications/.claude/skills/dev-server/`. The ai-notifications version manages a multi-binary workspace (hub, TTS, STT, agent-server, discord-bridge, Tauri app) with hub registration, audio crash-loop alerts, and dual orchestration modes (`dev-bin`, `dev-remote`). All of that was stripped here:

- Single-binary project: only `coyote-bin` + `vite` + legacy `tauri-dev`
- No hub/worker registration logic
- No crash-loop audio alerts (and no `assets/alerts/` directory)
- No `dev-remote` orchestration; only `dev` (compiled-binary + Vite HMR)
- Configurable port (`COYOTE_DEV_PORT`, default `9860`) to avoid clashing with ai-notifications's `9850`
- Vite probe added: `startViteAndHealth` skips spawn if the URL already responds, so a user-started `npm run dev` outside the daemon doesn't trigger a port-conflict crash loop
- Watchdog skip for crash-looped deps: prevents endless restart spam if a `requires` dep is unrecoverable

The daemon still respects the same plugin contract, so plugins can be ported between the two skills with only `ctx` field renames.
