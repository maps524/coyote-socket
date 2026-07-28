# Browser API Feasibility Matrix

Every capability the PWA needs, whether the browser can do it, and what it costs.

## Legend

- ✅ works today, no caveats worth worrying about
- ⚠️ works with a real constraint you must design around
- ❌ not possible in a browser

---

## 1. Talk to the Coyote 3.0 over Bluetooth LE

**Status: ✅ (Chrome Android / Chrome desktop / Edge)**

The device's GATT UUIDs are already known from `src-tauri/src/bluetooth.rs`:

| Purpose | UUID |
|---|---|
| V3 advertised service | `0000180c-0000-1000-8000-00805f9b34fb` |
| V3 instruction (B0/BF write) | `0000150a-0000-1000-8000-00805f9b34fb` |
| V3 battery | `00001500-0000-1000-8000-00805f9b34fb` |
| V2 service | `955a180b-0fe2-f5aa-a094-84b8d4f3e8ad` |
| V2 intensity / waveform A / B | `955a1504` / `955a1506` / `955a1505` (`-0fe2-f5aa-...`) |

**None of these appear on the Web Bluetooth GATT blocklist.** Verified against
`WebBluetoothCG/registries/gatt_blocklist.txt` — the blocklist contains only HID (`0x1812`),
Nordic DFU, TI OTA, Cypress bootloader, and the FIDO services. The Coyote is clear.

Throughput is a non-issue: the B0 command is ~20 bytes at 10 Hz. Android negotiates connection
intervals down to 7.5 ms, and `characteristic.writeValueWithoutResponse()` has been available
since Chromium 85 — use it for the hot path so writes don't block on ACK.

**Caveats:**

- Device selection goes through the browser's chooser dialog, gated on a user gesture. You cannot
  build your own scan UI or auto-connect on page load the way the Tauri app does.
- Reconnect across sessions relies on `navigator.bluetooth.getDevices()` (persisted permissions).
  Availability and reliability on Android must be measured — see `06-open-questions.md`.
- The GATT connection drops when the page is discarded. Design a fast reconnect path and a
  hard-zero on disconnect.

---

## 2. Read a Bluetooth game controller

**Status: ⚠️ works, but *not* via Web Bluetooth**

Use the **Gamepad API** (`navigator.getGamepads()`), which reads controllers the OS has already
paired. Chrome has supported it on Android since Chrome 21, with standard Xbox-style mapping and
`GamepadHapticActuator` rumble since Chrome 89.

You **cannot** do this over Web Bluetooth: HID (`0x1812`) is explicitly blocklisted, precisely to
stop pages from sniffing keyboards. So the flow is: *pair the controller in Android Bluetooth
settings → the PWA sees it through the Gamepad API.* Two pairing flows for the user (controller in
OS settings, Coyote in the browser chooser) — worth calling out in onboarding.

**Caveats:**

- Requires a secure context. Chrome now blocks the Gamepad API on plain HTTP.
- Requires a user gesture before `gamepadconnected` fires.
- Polling only — no events for axis movement. `requestAnimationFrame` loop, which stops when the
  tab is hidden (see §6).
- `src-tauri/src/gamepad.rs` is 1,300 lines built on native input; the mapping/binding logic is
  reusable, the transport is not.

---

## 3. Run the signal engine

**Status: ✅ — and you can reuse the existing Rust**

The DSP-critical modules are nearly free of platform dependencies:

| Module | LOC | Imports | WASM-ready? |
|---|---|---|---|
| `protocol.rs` | 152 | *(none)* | ✅ pure |
| `waveform.rs` | 23 | `serde` | ✅ pure |
| `modulation.rs` | 938 | `serde`, `crate::transforms` | ✅ pure |
| `resolver.rs` | 722 | `serde`, crate-internal | ✅ pure |
| `processing.rs` | 2,489 | `regex`, `serde`, `tokio::sync::RwLock` | ⚠️ swap `tokio::sync::RwLock` for `std::sync::RwLock` / `RefCell` |
| `device.rs` | 628 | `tokio::time`, `btleplug` | ❌ rewrite — this is the transport loop |

That's roughly **4,300 lines of portable signal path**. Compile it with `wasm-bindgen` and the PWA
gets bit-identical behaviour to the desktop app — same curves, same engines (v1 / v2-Smooth /
v2-Balanced / v2-Detailed), same B0/BF byte generation. This is far better than reimplementing the
engine in TypeScript and slowly drifting.

The parts that must be written fresh for the browser: the 10 Hz tick loop, the BLE writer, and the
settings persistence layer.

---

## 4. Load and play funscripts

**Status: ✅**

Funscripts are JSON (`{actions: [{at, pos}], ...}`). Parsing, interpolation (pchip/makima, as MFP
does), heatmap rendering on a `<canvas>` — all trivially browser-native. Position → axis value →
the existing modulation pipeline is exactly the same shape as the current T-Code input path, so
this becomes **a new input source, not a new subsystem.**

**File access caveats on Android:**

- `<input type="file" multiple>` works everywhere — user picks video + script together.
- The File System Access API (`showDirectoryPicker`) is **not available on Chrome Android**, so you
  cannot mount a local folder as a script library on a phone. Remote libraries (§5) or
  IndexedDB-cached uploads are the answer.
- Origin Private File System (OPFS) is available and is the right place to cache imported scripts.

---

## 5. Reach a script library / "funscript server"

**Status: ⚠️ depends entirely on whether the server sends CORS headers**

MFP has three script repositories, all HTTP:

| Repository | Transport | Browser-reachable? |
|---|---|---|
| Local folder | filesystem | ❌ on Android (no directory picker) |
| **Stash** | GraphQL over HTTP | ⚠️ likely — Stash is itself a web app; needs CORS + HTTPS |
| **XBVR** | REST over HTTP | ⚠️ likely — same reasoning |

Plus the obvious new option: **a purpose-built static script index.** A JSON manifest plus script
files on any static host (or an S3 bucket, or a GitHub Pages repo) with `Access-Control-Allow-Origin`
set. Zero server code. This should be the reference implementation.

The blocker for self-hosted Stash/XBVR on a LAN is not CORS — it's HTTPS. See §7.

---

## 6. Keep a 10 Hz loop alive

**Status: ⚠️ the sharpest constraint in the whole design**

Chrome throttles `setTimeout`/`setInterval` in hidden tabs to **1 Hz**, and after 5 minutes hidden
with chained timers, to **1/minute**. `requestAnimationFrame` stops entirely. A dedicated Web
Worker escapes main-thread throttling on desktop — but **on Chrome Android, a worker's timers are
also put to sleep roughly 5 minutes after the page is backgrounded.** The rezreal Coyote demo hit
exactly this and documents it.

Mitigations, in order of preference:

1. **Screen Wake Lock API** (`navigator.wakeLock.request('screen')`) — keeps the screen on, so the
   page stays visible and unthrottled. Available on Chrome Android. This matches actual usage:
   the phone *is* the controller and is in your hand.
2. **Run the tick in a Web Worker** anyway — protects against main-thread jank from UI rendering,
   and buys you the full 5 minutes if the screen does drop.
3. **`visibilitychange` → immediate hard-zero.** Non-negotiable safety behaviour: if the page is
   hidden and the loop can no longer be trusted, ramp the device to zero rather than leave it
   emitting the last commanded value. Belt and braces: a firmware-side watchdog if the Coyote
   supports one.

A silent AudioWorklet is the classic anti-throttling hack, but it is fragile, drains battery, and
is not worth shipping when Wake Lock covers the real use case.

---

## 7. Talk to a desktop video player on the LAN

**Status: ❌ for most players, ⚠️ for a few — this is where the bridge earns its keep**

See `03-media-sources.md` for the per-player breakdown. The general problem has two layers:

**Layer 1 — transport.** A browser can only speak HTTP(S), WebSocket, WebRTC, and WebTransport.
It cannot open a raw TCP socket. DeoVR (TCP 23554, length-prefixed JSON), HereSphere (TCP 23554),
Whirligig (TCP 2000), MPV (Windows named pipe), and PotPlayer (Win32 window messages) are therefore
**permanently out of reach**, WASM or not.

**Layer 2 — mixed content + CORS.** For the HTTP-based ones (VLC, MPC-HC, Plex, Emby, Jellyfin),
an `https://` PWA making a request to `http://192.168.1.50:8080` is blocked as mixed content, and
then blocked again by CORS since VLC and MPC-HC send no `Access-Control-Allow-Origin`.

Chrome has a partial escape hatch: the **Local Network Access permission** relaxes the mixed-content
check for requests it knows target the local network — a private IP literal, a `.local` hostname, or
`fetch(url, { targetAddressSpace: "local" })`. But it still requires the target to answer a preflight
with `Access-Control-Allow-Private-Network: true` **and** normal CORS headers. VLC will never send
those. A bridge you control will.

**Conclusion: a small local bridge is required for desktop players, and once you have one, it should
also handle the raw-TCP players.** See `02-architecture.md` for how to keep it optional and tiny.

---

## 8. Ship it as an installable PWA

**Status: ✅**

Manifest + service worker + install prompt on Chrome Android. Offline-capable for Tier A usage
(the WASM core, UI, and cached scripts all live in OPFS/Cache Storage). Note that a service worker
cannot host the tick loop — service workers are killed aggressively and have no timer guarantees.

---

## 9. iOS

**Status: ❌ for Safari, ⚠️ with workarounds**

Safari on iOS/iPadOS has no Web Bluetooth and Apple has shown no intent to add it — global Web
Bluetooth support sits around 76%, entirely from Chromium. Options:

- **Bluefy / WebBLE** — third-party iOS browsers that implement Web Bluetooth. Works, but asks the
  user to install a specific browser.
- **iOSWebBLE / beacio** — Safari web extensions polyfilling `navigator.bluetooth` onto CoreBluetooth.
  Newer, less proven.
- **Capacitor shell** with a native BLE plugin — the PWA codebase unchanged, wrapped for the App
  Store. But App Store review of an e-stim app is its own adventure.

Recommendation: **target Android, treat iOS as a later, separate decision.** Don't compromise the
architecture for it.

---

## Sources

- [Web Bluetooth GATT blocklist](https://raw.githubusercontent.com/WebBluetoothCG/registries/master/gatt_blocklist.txt)
- [Web Bluetooth implementation status](https://github.com/WebBluetoothCG/web-bluetooth/blob/main/implementation-status.md)
- [caniuse: Web Bluetooth](https://caniuse.com/web-bluetooth)
- [Heavy throttling of chained JS timers in Chrome 88](https://developer.chrome.com/blog/timer-throttling-in-chrome-88)
- [New permission prompt for Local Network Access](https://developer.chrome.com/blog/local-network-access)
- [PNA permission to relax mixed content](https://chromestatus.com/feature/5954091755241472)
- [WICG/local-network-access explainer](https://github.com/WICG/local-network-access/blob/main/explainer.md)
- [Chromium: require secure context for Gamepad API](https://issues.chromium.org/issues/40220668)
- [buttplug-wasm](https://www.npmjs.com/package/buttplug-wasm)
- [rezreal — Coyote Web Bluetooth example](https://rezreal.github.io/coyote/web-bluetooth-example.html)
- [dglab-deviant/coyote-3-studio](https://github.com/dglab-deviant/coyote-3-studio)
- [DeoVR remote control API](https://deovr.com/app/doc)
- [Punch Through — Maximizing BLE throughput on iOS and Android](https://punchthrough.com/maximizing-ble-throughput-on-ios-and-android/)
