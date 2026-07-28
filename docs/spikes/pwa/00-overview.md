# Spike: Browser-Only CoyoteSocket (PWA)

**Date:** 2026-07-27
**Status:** Recon complete — no code written
**Author:** research spike, for consumption by other agents

## The goal

Today a session needs three applications:

1. **MultiFunPlayer (MFP)** — reads video player position, plays funscripts, emits T-Code
2. **CoyoteSocket** (this repo) — receives T-Code, runs the signal engine, drives the Coyote over BLE
3. **A video player** — VLC / DeoVR / HereSphere / MPV

Target: **one phone, one browser tab.** A PWA that:

- connects to the Coyote 3.0 directly over Bluetooth LE
- reads a Bluetooth game controller
- loads funscripts from a script library ("funscript server")
- syncs to playback position (either an in-app player or an external one)
- converts funscript position → T-Code-equivalent axis values → existing signal engine → BLE

## Verdict

**Feasible, in three tiers.**

| Tier | What works | Servers needed |
|---|---|---|
| **A — Self-contained PWA** | BLE output, gamepad input, funscript playback, video played *inside* the PWA, full signal engine | **none** |
| **B — Networked script/media libraries** | Jellyfin / Plex / Stash / XBVR / OpenFunscripter as sources | their own servers, already CORS-capable |
| **C — Desktop video players** | VLC, MPV, MPC-HC, DeoVR, HereSphere, Whirligig, PotPlayer | **a small local bridge is unavoidable** |

Tier A is the big win: it removes MFP *and* the video player in one move, and it is 100% browser-native on Chrome Android. Tier C cannot be done from a browser at any price — those players speak raw TCP, named pipes, Win32 window messages, or CORS-less HTTP. No amount of WASM changes that: **WASM has no socket access.** It runs in the same sandbox as JS.

## The three hard constraints

1. **Web Bluetooth and the Gamepad API both require a secure context (HTTPS).**
   Consequence: the PWA is served over `https://`, which means it *cannot* make plain `http://` or `ws://` requests to LAN devices without either Chrome's Local Network Access permission (+ cooperating server headers) or the target speaking HTTPS. This is the single most important architectural fact in this document.

2. **iOS Safari has no Web Bluetooth and will not get it.**
   But **Bluefy on iOS works** — measured, see `07-measured-results.md`. The Coyote connects in
   under a second with `writeWithoutResponse` available. No native shell needed for output.

3. **Backgrounded tabs are throttled.**
   Wake Lock keeps the screen on while the page is visible, which matches how the app is actually
   used — phone in hand. Measured on iOS, the loop survived backgrounding far better than expected
   (a live radio link through app switching and YouTube playback), but that time is granted at the
   OS's discretion and revocable. **Hidden still means ramp to zero.**

4. **Web Bluetooth cannot run in an iframe** without `allow="bluetooth"`. The API appears present
   and every call hangs forever. This cost hours in the spike — see `07-measured-results.md`.

## Documents in this spike

| File | Contents |
|---|---|
| `01-feasibility-matrix.md` | Every browser API needed, with support status and evidence |
| `02-architecture.md` | Proposed system design, WASM core reuse, module map |
| `03-media-sources.md` | MFP's 12 media sources dissected, browser reachability per source |
| `04-plugin-system.md` | Extensibility design for community-contributed sources/outputs |
| `05-roadmap.md` | Phased plan, effort estimates, risk register |
| `06-open-questions.md` | Things a follow-up spike must actually measure |
| `07-measured-results.md` | **Measured on real hardware — read this first.** iOS/Bluefy results and three portability traps |
| `08-v1-scope.md` | **The build spec.** Decided V1 scope — UI, script library, repo layout |
| `09-quest-and-players.md` | Quest Browser ruled out, DLNA ruled out, DeoVR/HereSphere bridge topology |
| `10-production-bugs.md` | **Defects found in the shipping desktop app**, including one safety bug |
| `probe/` | The capability probe itself, plus how to serve it |

> **Status:** Phase 0 is complete and every question came back yes, including on iOS, which this
> document originally wrote off. Phase 1 is unblocked. `07-measured-results.md` supersedes any
> inference in the files below where they disagree.

## Prior art worth reading before writing code

- **rezreal/coyote** — a working Web Bluetooth demo that drives a Coyote from a browser tab. It independently hit the background-throttling problem and documents it. <https://rezreal.github.io/coyote/web-bluetooth-example.html>
- **dglab-deviant/coyote-3-studio** — browser UI driving Coyote 3.0 over BLE, including a funscript player. Python backend + plain HTML/JS.
- **buttplug-wasm** — the Buttplug server compiled to WASM, driving toys via Web Bluetooth from a page. Proves the whole "Rust engine + Web Bluetooth in a tab" pattern works, and is a possible output target for free.
- **MultiFunPlayer** (Yoooi0) — the thing being replaced. Its `MediaSource/`, `Script/Repository/`, and `Plugin/` directories are the reference designs to steal from.
