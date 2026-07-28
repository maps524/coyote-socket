# Proposed Architecture

## Guiding principle

**Everything that can run in the tab, runs in the tab.** The bridge is an optional accessory for
people who want to drive a desktop video player — not a dependency of the product.

## System shape

```
┌─────────────────────────────────────────────────────────────┐
│  PHONE — Chrome Android, installed PWA (https origin)        │
│                                                              │
│  ┌────────────┐   ┌──────────────┐   ┌───────────────────┐  │
│  │ INPUT      │   │ CORE (WASM)  │   │ OUTPUT            │  │
│  │            │──▶│              │──▶│                   │  │
│  │ funscript  │   │ modulation   │   │ Web Bluetooth     │──┼──▶ Coyote 3.0
│  │ gamepad    │   │ processing   │   │ (B0/BF writes)    │  │
│  │ manual UI  │   │ resolver     │   │                   │  │
│  │ plugins    │   │ protocol     │   │ buttplug-wasm     │──┼──▶ other toys
│  └────────────┘   └──────────────┘   └───────────────────┘  │
│         ▲                 ▲                                  │
│         │            10 Hz tick (Web Worker + Wake Lock)     │
│  ┌──────┴───────────────────────────────────────────────┐   │
│  │ SYNC SOURCE — where is playback right now?            │   │
│  │  a) internal <video>            (no network)          │   │
│  │  b) CORS-capable server         (Jellyfin/Stash/…)    │   │
│  │  c) bridge                      (wss://)              │   │
│  └───────────────────────────────────────────────────────┘   │
└──────────────────────────────────┬───────────────────────────┘
                                   │ optional, only for (c)
┌──────────────────────────────────┴───────────────────────────┐
│  MEDIA PC — coyote-bridge (small Rust binary, ~1 file/player) │
│  raw TCP / named pipe / HTTP  ──▶  VLC, MPV, DeoVR, HereSphere│
└───────────────────────────────────────────────────────────────┘
```

## Layer by layer

### Core — reuse the Rust, compiled to WASM

Do **not** reimplement the signal engine in TypeScript. Extract the pure modules into a
`coyote-core` crate consumed by both the Tauri app and the PWA:

```
src-tauri/          → thin Tauri shell (BLE via btleplug, tokio loop, settings)
crates/coyote-core/ → protocol, waveform, modulation, resolver, transforms, processing
crates/coyote-wasm/ → wasm-bindgen wrapper over coyote-core
```

Work needed to make `coyote-core` build for `wasm32-unknown-unknown`:

- `processing.rs` uses `tokio::sync::RwLock`. Replace with a lock abstraction, or restructure so the
  core is a synchronous `step(dt, inputs) -> outputs` function and *callers* own the concurrency.
  The latter is cleaner and makes the core trivially testable.
- Replace `current_time_ms()` with an injected timestamp — the caller passes the tick time. This
  also removes `Date.now()`-style nondeterminism from the tests.
- `regex` compiles to WASM fine but bloats the bundle; check whether the one use in `processing.rs`
  can be a hand-rolled parser.

Payoff: **one signal engine, two shells.** A curve tweak lands in both apps. Behaviour is identical
by construction rather than by discipline.

### Tick loop

Runs in a **dedicated Web Worker**, not the main thread:

- `setInterval` at 100 ms inside the worker, drift-corrected against `performance.now()`
- worker holds the WASM instance and the `SharedArrayBuffer`-backed input snapshot (requires COOP/COEP
  headers — verify this doesn't conflict with anything else you serve; `postMessage` is an acceptable
  fallback at 10 Hz)
- **BLE writes must happen on the main thread** — Web Bluetooth is not exposed to workers. The worker
  posts a 20-byte command; the main thread writes it. At 10 Hz, `postMessage` overhead is noise.
- Main thread holds the Screen Wake Lock while a session is active, and releases it on stop.

### Safety layer (non-negotiable)

This app drives e-stim hardware. The browser's execution model is *less* reliable than a native
process, so the safety envelope has to be stricter, not looser:

- **Deadman:** the BLE writer tracks the last successful write. If the worker misses N ticks, or
  `document.visibilityState` goes `hidden`, or the Wake Lock is lost, ramp to zero immediately.
- **Rate limit on increase:** the existing engine already ramps; make sure the WASM boundary can't
  be re-entered with a stale-but-high value after a stall.
- **Disconnect = zero.** On `gattserverdisconnected`, do not attempt a silent reconnect-and-resume;
  reconnect at zero and require a user action to resume.
- **Clamp in the core, not the UI.** Limits belong on the WASM side of the boundary where a plugin
  cannot route around them.

### Input abstraction

The current app already has the right concept: a T-Code axis value feeding `ParameterSource`. Keep
that. A funscript is simply a new producer of axis values:

```ts
interface InputSource {
  id: string
  // called each tick with the current session clock
  sample(tMs: number): Partial<Record<AxisId, number>>  // L0..L2, R0..R2 → 0..1
}
```

- `FunscriptInput` — interpolates the loaded script at `tMs` (pchip, matching MFP)
- `GamepadInput` — polls `navigator.getGamepads()`, applies the existing binding model
- `TCodeWebSocketInput` — for parity with today, if a bridge or LAN app wants to push T-Code
- plugin-provided sources — anything the community dreams up

This is the answer to "playback is just a new input format." It is, almost literally, a drop-in.

### Sync source abstraction

Separate from input: *what time is it in the media?*

```ts
interface SyncSource {
  id: string
  connect(): Promise<void>
  // emits position updates; the app interpolates between them
  onState(cb: (s: { path: string; positionMs: number; playing: boolean; durationMs: number }) => void): void
  seek?(ms: number): Promise<void>
  playPause?(play: boolean): Promise<void>
}
```

Three implementations to start:

1. **`InternalPlayer`** — an in-app `<video>`. Zero network. `timeupdate` fires ~4 Hz, so interpolate
   from `currentTime` + `performance.now()` deltas and resync on `seeked`/`ratechange`.
2. **`HttpPollingSync`** — generic poller for CORS-capable servers (Jellyfin, Plex, Stash, XBVR).
3. **`BridgeSync`** — WebSocket to `coyote-bridge`, which normalizes every desktop player to the
   same state shape.

MFP's `AbstractMediaSource` proves this abstraction holds across 12 wildly different players — copy
the message set (`MediaPathChanged`, `MediaPlayPause`, `MediaSeek`, `MediaPositionChanged`,
`MediaDurationChanged`, `MediaSpeedChanged`), it's well designed.

### Script library abstraction

```ts
interface ScriptRepository {
  id: string
  // given a media path/identifier, return candidate scripts per axis
  resolve(media: MediaRef): Promise<Record<AxisId, ScriptRef>>
  fetch(ref: ScriptRef): Promise<Funscript>
}
```

Implementations: `OpfsLibrary` (user-imported, cached locally), `StaticIndexRepository` (a
`index.json` on any static host — the reference implementation), `StashRepository`,
`XbvrRepository`, plugin-provided.

### The bridge — `coyote-bridge`

Optional, single small Rust binary, run on the machine playing the video. Its whole job:

- speak the native protocol to a desktop player (MFP's `MediaSource` implementations are ~200–400
  lines each and **MIT licensed** — they can be ported directly with attribution)
- expose one normalized WebSocket endpoint
- serve the CORS and Private Network Access headers the PWA needs

**The HTTPS problem, and how to solve it.** The PWA is `https://`, so it needs `wss://`. Options,
best first:

1. **Shipped wildcard cert (the Plex model).** Register `*.bridge.<yourdomain>`, publish DNS records
   that resolve `192-168-1-50.bridge.<yourdomain>` → `192.168.1.50`, ship the cert with the bridge.
   The PWA connects to `wss://192-168-1-50.bridge.<yourdomain>:7890`. Works today, no flags, no
   prompts. Cost: you're distributing a private key publicly — it can be revoked, and you're
   on the hook for renewals and a DNS zone. This is exactly what Plex does and it has held up.
2. **Local Network Access permission + plain HTTP.** Chrome-only, and the WebSocket story is
   unresolved — the mixed-content exemption is documented for `fetch()` with `targetAddressSpace`,
   and `WebSocket` has no equivalent option. Might force you onto SSE or long-polling over `fetch`.
   **Must be measured before committing** (see `06-open-questions.md`).
3. **Bridge serves the PWA itself over its own HTTPS.** Simplest for a user who's already installing
   a binary, but then it's not really "just a web app" anymore.

Recommendation: build the PWA against option 1's interface, prototype option 2 in the spike, and
keep the bridge's transport swappable.

### Persistence

- Settings, presets, bindings → IndexedDB (mirror the existing `settings.json` schema so configs
  are portable between desktop and PWA)
- Imported scripts/videos → OPFS
- Export/import a config bundle as a single JSON file, matching the desktop app's format

## What this buys you

| Today | With Tier A PWA |
|---|---|
| MFP + CoyoteSocket + VLC on a PC | one browser tab on a phone |
| T-Code over WebSocket between two apps | in-process function call |
| Windows-only | any Chromium device |
| Install two binaries | open a URL, tap install |

## What it costs you

- A second BLE code path to maintain (btleplug and Web Bluetooth) until/unless the Tauri app is retired
- A WASM build in CI, plus the core-extraction refactor
- The bridge, if you want desktop player support — a third artefact
- No iOS
