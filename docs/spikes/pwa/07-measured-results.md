# Measured Results — iOS

Real hardware, not inference. Produced by `probe/index.html` (see `probe/README.md` for how to
run it).

**Rig:** iPhone, iOS 18.7, Bluefy 3.9.3 (`Version/3.9.3 Bluefy/3.9.3`), DG-LAB Coyote 3.0
advertising as `47L121000`. Page served from a local Node server behind a Cloudflare quick tunnel,
so the origin was top-level HTTPS with a publicly trusted certificate.

**Date:** 2026-07-28

---

## Headline

**Every Phase 0 question came back yes, on the platform this spike had written off.**

The original plan assumed Android-only and treated iOS as a later Capacitor decision. That was
wrong. iOS via Bluefy drives the Coyote today — connection, analog controller input, a stable
10 Hz loop, and a genuinely live radio link even while backgrounded.

| Question | Result |
|---|---|
| Connect to the Coyote from an iOS browser | ✅ 891–912 ms |
| `writeWithoutResponse` on the instruction characteristic | ✅ |
| Analog trigger input | ✅ continuous, many distinct levels |
| Screen Wake Lock | ✅ granted, behaves to spec |
| 10 Hz loop, foreground | ✅ |
| 10 Hz loop, screen locked | ✅ with looping silent audio |
| 10 Hz loop, app backgrounded | ✅ survived browsing and YouTube playback |
| Radio link alive while hidden | ✅ **round-trip reads succeeded, zero failures** |

---

## Bluetooth

Full GATT layout as reported by the phone:

```
service 180C
   char 150B  [notify]            <- device notify channel
   char 150A  [writeNoResp]       <- B0/BF instruction characteristic
service 180A
   char 1501  [read]
   char 1502  [read]
   char 1500  [read,notify]       <- battery
   char 2A59  [read,notify]
service 2004
   char 0009  [read,write]
service 2003
   char 0007  [write]
   char 0008  [read,notify]
service FE59
   char 8EC90003-F315-4F60-9FB8-838830DAEA50  [write,indicate]   <- Nordic DFU
```

Matches the UUIDs in `src-tauri/src/bluetooth.rs`. `filters: [{ namePrefix: '47L' }]` matches on
the first attempt.

## Gamepad

Triggers are **buttons 6 and 7** in the standard mapping, carrying a continuous `.value` from 0 to
1 — not axes, and not booleans. Confirmed analog on iOS with many distinct intermediate levels.

This matters for the product: an analog trigger is a 0..1 continuous signal, structurally identical
to a funscript position or a T-Code axis. It drops into the existing `ParameterSource` model with
no special casing. **Squeeze-to-intensity is a first-class input, not a bolt-on.**

## Background execution

Measured across several runs with a looping silent WAV playing:

| Hidden for | Ticks | Writes | Write errors | Round-trip reads |
|---|---|---|---|---|
| 37 s | 353 | 352 | 0 | — |
| 94 s (incl. YouTube playback) | 944 | 942 | 0 | — |
| 14 s | 218 | 218 | 0 | 5 ok, 0 failed |

Tick counts are cumulative across hidden periods. The loop kept running while the app was
backgrounded, while other apps were in use, and while YouTube was playing audio.

**Why the round-trip reads matter.** `writeValueWithoutResponse` is fire-and-forget: it resolves
once the write is queued in the local BLE stack and never confirms the peripheral received
anything. A stalled radio would produce a write counter that looks perfectly healthy. Periodic
battery `readValue()` calls require a real request/response exchange with the device, so their
success while hidden is what actually proves the link is alive.

### This does not change the safety design

Background execution here is granted by iOS at its discretion and can be revoked mid-session
without warning. **Hidden still means ramp to zero.** What the measurements buy is a robustness
margin — the app degrades gracefully rather than dying instantly — not permission to depend on
background operation. Do not build a feature on top of this.

---

## Three findings that will bite the real app

### 1. Web Bluetooth is blocked in iframes without `allow="bluetooth"`

This cost the most time in the spike by a wide margin.

Web Bluetooth is gated by Permissions Policy. In a cross-origin iframe that does not carry
`allow="bluetooth"`, **`navigator.bluetooth` still exists, `getAvailability()` still returns true,
and `requestDevice()` never settles.** No error, no rejection — the promise simply hangs forever.

The symptom is indistinguishable from a broken browser, a bad device, or a filter bug. It sent this
spike chasing Bluefy quirks, iOS pairing state and UUID filters for over an hour before the
environment itself turned out to be the cause.

Consequences:

- A Claude artifact, CodePen, embedded preview, or any iframe-based host **cannot** run this. The
  page must be a top-level document on an origin you control.
- **Always wrap GATT calls in a timeout.** A hung promise with no error is the worst possible
  failure mode on a phone with no devtools.

### 2. UUID form differs between implementations

Chrome returns canonical 128-bit lowercase UUIDs (`0000150a-0000-1000-8000-00805f9b34fb`).
**Bluefy returns the short 16-bit form, uppercase** (`150A`). A direct comparison against the
canonical form silently matches nothing — the probe reported "no 150a characteristic found" while
the characteristic was sitting right there in its own log output.

```js
function canon(uuid) {
  const u = String(uuid).toLowerCase();
  if (/^[0-9a-f]{4}$/.test(u)) return '0000' + u + '-0000-1000-8000-00805f9b34fb';
  if (/^[0-9a-f]{8}$/.test(u)) return u + '-0000-1000-8000-00805f9b34fb';
  return u;
}
```

Anything looking up characteristics by UUID needs this, or it works on Chrome and fails
mysteriously on iOS.

### 3. Wake Lock releases when the page hides — by design

`navigator.wakeLock.request('screen')` prevents **auto-lock while the page is visible**. It is
released the instant the page becomes hidden. That is spec-correct, not a bug, and it is exactly
what the app wants: the phone is the controller and is in the user's hand.

For belt and braces, a looping silent video (the NoSleep.js trick) also holds the screen awake and
works as a fallback.

### Retracted

An earlier note in this spike claimed Bluefy mishandles `requestDevice` filters, based on a W3C
issue report and an empty chooser. **That was wrong** — the empty chooser was finding 1 above.
With a proper origin, name-prefix filtering worked immediately.

---

## Serving the page during development

Bluefy will not accept a self-signed certificate, and Web Bluetooth requires a secure context, so
`http://<lan-ip>` is not an option either. What worked:

```bash
PLAIN=1 PORT=8090 node docs/spikes/pwa/probe/serve.mjs
cloudflared tunnel --url http://localhost:8090
```

The quick tunnel gives a publicly trusted `https://….trycloudflare.com` origin with no account and
no signup. The server re-reads the file per request, so edits appear on refresh.

This is also a working preview of the Phase 3 certificate problem: a tunnel, or a Plex-style
shipped certificate, is how the bridge will have to present itself.

---

## What this changes about the plan

- **iOS is not a later decision.** It works now, via Bluefy, with no native shell. The Capacitor
  path stays in the back pocket for a nicer install story, not as a prerequisite.
- **Keep the transport behind an interface anyway.** Three implementations remain plausible: Web
  Bluetooth, btleplug on desktop, native CoreBluetooth if Bluefy ever proves limiting.
- **beacio remains interesting** for iOS 26+ — a Safari extension polyfill would give a real
  installable PWA instead of a separate browser. Not available on iOS 18.7.
- **Phase 1 is unblocked.** No remaining platform unknown stands between here and building the
  self-contained app.

## Still unmeasured

- Long-session stability (hours, not minutes) and thermal/battery behaviour
- End-to-end latency distribution under real funscript playback
- Whether the Coyote tolerates browser timer jitter at sustained non-zero output
- Reconnect after the page is discarded, and `getDevices()` permission persistence on iOS
