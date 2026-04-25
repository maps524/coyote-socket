---
name: dev-server
description: "Build, run, and manage the Coyote-Socket dev environment. Use for ALL building and compiling (never run `cargo build` or `npx tauri dev` directly). Handles Vite + compiled-binary mode with shadow-copy hot-swap so frontend HMR keeps working across Rust rebuilds, plus a legacy `tauri dev` fallback. Provides log streaming, multi-agent build locking, and a TUI dashboard. Start the dev server if it's not running. Do NOT stop it unless explicitly asked."
allowed-tools: Bash
---

# Dev Server Skill

Manages the Coyote-Socket dev environment via a background daemon. Supports build locking, shadow-copy hot-swap, log aggregation, and a live TUI dashboard.

## Architecture

The **default mode** runs Vite (Svelte/CSS/TS HMR on `:1421`) plus a **release-built `coyote-socket` binary** that loads the frontend from Vite via the `DEV_URL` env var. When you trigger a Rust rebuild, the daemon stops the binary, copies a fresh `coyote-socket.exe` over `.active.exe`, and starts the shadow copy — Vite stays up the entire time, so the frontend is never reloaded and app state is preserved.

The **legacy mode** (`tauri-dev`) runs the standard `npx tauri dev` watcher, which auto-rebuilds Rust on file changes but kills the window every time.

## Services

| Name | Alias | Description |
|------|-------|-------------|
| `vite` | — | Vite dev server on `:1421` (HMR) |
| `coyote-bin` | `bin`, `coyote` | Compiled `coyote-socket` binary, shadow-copy hot-swap |
| `tauri-dev` | `tauri` | Legacy `npx tauri dev` (auto-rebuilds Rust on file changes) |

`coyote-bin` and `tauri-dev` share the same `tauri` group → mutually exclusive.

## CLI

```bash
node .claude/skills/dev-server/cli.mjs <command> [name] [flags]
```

Or use the npm aliases (avoids the long path):

```bash
npm run dev            # build + start Vite + coyote-bin (shadow-copy mode)
npm run dev:dash       # live TUI dashboard
npm run dev:status     # JSON status snapshot
npm run dev:tail       # follow all logs
npm run dev:logs       # last 50 log lines
npm run dev:build      # rebuild coyote-socket
npm run dev:stop       # stop all services (daemon stays up)
npm run dev:kill       # kill daemon + all services
npm run dev:tauri      # start legacy tauri-dev mode
npm run dev:vite       # bare \`vite\` (used internally by Tauri's beforeDevCommand)
```

| Command | Description |
|---------|-------------|
| `status` | Service overview (default) |
| `start [name]` | Start a service (default: `coyote-bin`) |
| `stop [name]` | Stop a service (default: all) |
| `restart [name]` | Restart a service |
| `logs [name] [flags]` | Service logs (default: all interleaved, last 50) |
| `tail [name]` | Continuous log stream (human use) |
| `build <name>` | Build a target. Auto-restarts the linked service after success. |
| `clean <name> [--force]` | Clean build artifacts. Stops/restarts linked service. |
| `dev [--rebuild]` | Build coyote-socket + start Vite + start binary (default mode) |
| `dashboard` (`dash`) | Live TUI |
| `help` | Full help |
| `shutdown` | Kill all services + daemon |

### Build Targets

| Target | What it builds | Linked service |
|--------|---------------|----------------|
| `coyote-bin` (aliases: `coyote`, `bin`) | `cargo build --release` (in `src-tauri/`) | `coyote-bin` |

When a build succeeds, the daemon stops the linked service, swaps in a fresh shadow copy, and restarts. If another agent triggers a rebuild while one is running, the in-progress build is killed and restarted to include the new changes — both agents observe the restart in the shared build log.

### Clean Targets

| Target | Crate | Notes |
|--------|-------|-------|
| `coyote-bin` (aliases: `coyote`, `tauri`) | `coyote-socket` | Safe, ~2 min rebuild from cold cache |

Clean acquires the build lock. If the linked service is running, it stops before cleaning and restarts after.

### Log Flags

| Flag | Description |
|------|-------------|
| `--since N` | Lines since log index N (incremental polling) |
| `--limit N` | Last N lines |
| `--level L` | Filter: `stdout`, `stderr`, `daemon` |
| `--all` | All buffered lines (up to 2000) |

All commands except `tail`, `dashboard`, and `help` output JSON.

### Persistent log files

In addition to the in-memory ring buffer, every service log is written to disk at `.claude/skills/dev-server/state/logs/`. Each service gets its own file (e.g. `coyote-bin.log`, `vite.log`, `build.log`). Files rotate at 2 MB (old content moves to `.prev.log`).

These survive daemon and service restarts — agents can grep them directly:

```bash
grep -i "websocket" .claude/skills/dev-server/state/logs/coyote-bin.log
tail -100 .claude/skills/dev-server/state/logs/build.log
```

## Agent Rules

1. **Before any build or compile check**: run `status` first. If `coyote-bin` is already running, the code is already compiled — read its logs instead of rebuilding.
2. **If the dev server is not running**: start it with `dev` (or `start coyote-bin` if the binary is already built).
3. **To check compilation**: use `logs` or `logs --since N`. Look for `error[E` (Rust) or `ERROR` (Vite).
4. **Do NOT stop services** unless the user explicitly asks, or a service is in a crash loop.
5. **Do NOT run `cargo build`, `cargo run`, or `npx tauri dev` directly** — always go through this skill.
6. **Build**: use `build coyote`. If another build is running, the daemon restarts it to include your changes — both agents see the restart in the shared log stream.
7. **Shadow copy** (`coyote-bin` only): the running binary is `coyote-socket.active.exe`; the build target `coyote-socket.exe` is never file-locked, so cargo can always overwrite it. After a successful build, the daemon swaps + restarts.
8. **Incremental polling**: save the `total` from a logs response, then pass it as `--since` next time to get only new entries.

## Dashboard

Run `npm run dev:dash` (or `node .claude/skills/dev-server/cli.mjs dashboard`) in any terminal for a live TUI.

| Key | Action |
|-----|--------|
| `1` | Toggle log filter: vite |
| `2` | Toggle log filter: coyote-bin |
| `3` | Toggle log filter: build output |
| `a` | Show all logs (clear filter) |
| `b` | Build coyote-bin |
| `d` | Start `dev` orchestration |
| `r` | Reload plugins from disk |
| `R` | Restart all running services (Shift) |
| `S` | Stop all services (Shift — destructive) |
| `c` | Clean (sub-menu) |
| `q` | Quit dashboard (daemon keeps running) |
| `K` | Kill daemon + quit |

## Daemon

Listens on `127.0.0.1:9860` by default — override with `COYOTE_DEV_PORT`. (Picked to not collide with the ai-notifications skill's daemon on `:9850`.) Auto-started by the CLI. Buffers up to 2000 log lines per service in memory. Runs as a hidden background process (no console window). Stale shadow copies are cleaned on daemon boot.

## Environment

| Var | Default | Effect |
|-----|---------|--------|
| `COYOTE_DEV_PORT` | `9860` | Daemon HTTP port |
| `COYOTE_VITE_URL` | `http://localhost:1421` | URL injected as `DEV_URL` into the compiled binary |

## DEV_URL contract (Rust side)

The compiled `coyote-socket` binary checks `DEV_URL` at startup (`src-tauri/src/main.rs` setup hook). When set, it navigates the `main` and `splashscreen` windows to that URL instead of using bundled assets. This is what makes shadow-copy hot-swap useful — the release-built binary becomes a thin shell loading frontend from Vite for HMR.
