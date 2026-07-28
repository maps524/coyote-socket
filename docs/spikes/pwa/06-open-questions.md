# Open Questions — Things This Spike Did Not Measure

Everything below is **inferred from documentation, not observed**. Any of them could change the
plan. They are ordered by how much damage a wrong assumption does.

## Blocking — answer before committing to the architecture

### 1. Does a `wss://` connection to a LAN bridge actually work without a public cert?

Chrome's Local Network Access permission exempts local-network requests from the mixed-content
check, but the documented mechanism is `fetch()` with `targetAddressSpace: "local"`. **`WebSocket`
has no equivalent option.** If `ws://` from an `https://` page is still blocked even with the LNA
permission granted, the bridge is forced onto one of:

- a publicly-trusted cert distributed with the binary (the Plex `*.plex.direct` model)
- SSE or long-polling over `fetch()` instead of WebSocket
- serving the PWA from the bridge itself

**Test:** minimal Rust WS server on `192.168.x.x`, an `https://` page trying to connect, on current
Chrome Android and desktop. Try `ws://`, `wss://` with a self-signed cert, and `fetch` +
`targetAddressSpace`.

### 2. What really happens to a 10 Hz loop when the phone screen goes off?

Documented: main-thread timers throttle to 1 Hz, then 1/min after 5 minutes; workers survive on
desktop but sleep after ~5 minutes on Chrome Android. Wake Lock should prevent the whole situation.
Unmeasured: behaviour on incoming call, notification shade pull-down, app switch, low-battery mode,
and OEM battery managers (Samsung/Xiaomi are aggressive).

**Test:** log tick timestamps to IndexedDB for 30 minutes across every one of those scenarios.
**Safety-critical:** confirm the deadman fires and the device goes to zero in each.

### 3. Is Web Bluetooth reconnect good enough for daily use?

`navigator.bluetooth.getDevices()` returns previously-permitted devices, but availability and
behaviour vary by Chrome version and platform, and `watchAdvertisements` has had a rocky history.
If every session needs a chooser dialog tap, that's tolerable; if permissions don't persist across
PWA restarts at all, the UX degrades badly.

**Test:** grant, close the PWA, cold start, attempt silent reconnect. Repeat after a reboot.

### 4. Does `coyote-core` extract cleanly?

`processing.rs` (2,489 lines) holds `tokio::sync::RwLock` and a global-ish state pattern. The refactor
to a synchronous, injectable-time core is assumed to be mechanical. If it turns out that processing
state is deeply entangled with the tokio runtime and the Tauri event emitter, this estimate is wrong.

**Test:** attempt the extraction on a branch, time-boxed to one day, and see how far it gets.

## Important — affect scope, not viability

### 5. Do Stash and XBVR send usable CORS headers?

Both are self-hosted web apps with browser front-ends, so they very likely allow their own origin —
but allowing a *third-party* origin is a different setting. If they don't, users need a reverse
proxy, or these repositories move behind the bridge.

### 6. Do Jellyfin / Emby / Plex allow cross-origin API access from an arbitrary PWA origin?

Same question. Plex's `*.plex.direct` cert scheme suggests it's set up for this; Jellyfin has a
configurable CORS policy. Needs verification against real instances, not docs.

### 7. What's the real end-to-end latency budget?

Funscript sample → WASM → `postMessage` → `writeValueWithoutResponse()` → device response. Each hop
is small, but BLE on Android shares radio time with WiFi and is variable. If total latency exceeds
~50 ms and jitters, sharp funscript transitions will feel mushy.

**Test:** instrument a full loop and histogram it over a session.

### 8. Does the Coyote tolerate a browser-paced write cadence?

The desktop app writes from a tokio interval with tight timing. Browser timers jitter. Does the
device stutter, or does its own buffering absorb it? rezreal's demo suggests it stutters below ~1 Hz;
unknown at 10 Hz ± jitter.

### 9. SharedArrayBuffer / COOP+COEP

If the worker↔main-thread channel wants `SharedArrayBuffer`, the site needs cross-origin isolation
headers, which break embedding third-party content and can complicate plugin loading. At 10 Hz,
`postMessage` is almost certainly sufficient — confirm and then avoid COOP/COEP entirely.

## Worth knowing eventually

### 10. Does OpenFunscripter's WebSocket server accept cross-origin connections?

WebSocket handshakes aren't subject to CORS, but servers can check `Origin`. If OFS doesn't, and if
question 1 resolves favourably, OFS integration is nearly free.

### 11. Can buttplug-wasm be used as an output target directly?

It runs a full Buttplug server in the tab over Web Bluetooth. If it can coexist with your own
`navigator.bluetooth` usage in the same page, multi-toy support arrives essentially for free.

### 12. Where does the funscript library actually live?

The "funscript server" in the original idea is underspecified. A static JSON index on any host is
the cheapest thing that works and should be the reference implementation — but does the user want
their library on their phone, on a NAS, or fetched from a community source? This is a product
question, not a technical one, and it shapes the repository interface.

### 13. iOS decision

Not now. But record what would trigger revisiting it: Bluefy adoption, an iOS user base, or a
Capacitor shell becoming worthwhile for other reasons.
