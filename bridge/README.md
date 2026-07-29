# coyote-bridge — spike

A minimal process that sits between a VR video player and a phone browser.

```
Quest (HereSphere or DeoVR, TCP :23554)
   ▲
   │ raw TCP — the bridge dials in
   │
Bridge (this)  ── serves the PWA over HTTP, relays player state over WebSocket
   │
   ▼
Phone (PWA)  ──Bluetooth──▶  Coyote
```

This is a **spike**, not a product. Its job was to answer one question — can we
actually talk to a player on 23554? — and to leave the surrounding scaffolding
in a state worth keeping.

## What is proven and what is not

**Updated 2026-07-28: the bridge has now talked to a real player.** DeoVR on a
Meta Quest, over Wi-Fi — one recording spanning **837.9 seconds** across
**three connections**, with **418 inbound frames**. Position streamed; play,
pause, forward and back all worked from the bridge. The raw capture is
committed at `fixtures/deovr-quest-2026-07-28.wire.jsonl` and what it settles
is in `src/capture.rs`.

> An earlier draft of this file described "two sessions, ~7 minutes, 240
> packets". That was measured from a capture file while it was still being
> written, and it understated the recording. Every quantitative claim below is
> now re-derived from the committed fixture by tests in `capture.rs`, so the
> prose fails the build rather than drifting from the evidence again.

| Part | Status |
|---|---|
| Length-prefixed framing | **Confirmed against a real DeoVR** — 418 of 418 frames parsed. |
| Byte order (little-endian) | **Measured**, not inferred. The capture records the raw prefix bytes: `c6 00 00 00` for a 198-byte payload, 418 times over. Big-endian would read that as 3,321,888,768. |
| Keepalive, timeout, reconnect | Tested against `fake-player`; the real link survived minutes without being dropped. |
| Position, duration, media identity | **Confirmed against a real DeoVR.** |
| Play / pause / seek from the bridge | **Confirmed against a real DeoVR**, by hand. |
| Static file serving | Tested, and run by hand against a directory of files. |
| WebSocket relay | Tested, and driven by hand from a desktop browser. **Never driven from a phone.** |
| `playerState` as a status field | **Confirmed unreliable.** See below. |
| **HereSphere, anything** | **Never connected to. Entirely unobserved.** |

Scope that precisely: **one player, one version, one platform, and only the
features that session exercised.** Seeking, playing and pausing were exercised.
Media changes, HereSphere, and on-device (non-streamed) media were not. This is
enormously more than the spike had; it is not "verified".

### The finding: `playerState` is advisory, not a status

DeoVR's documented mapping (`Play = 0, Pause = 1`) is **correct** — every pause
in the capture coincides with `1`, every stretch of `0` advances at 1.0×.
Nothing in the client or the fake was changed.

But the field does not observe the player. It appears to **echo the last value a
remote client set**:

```
t+  0 …  58 s   state 0    advancing 1.00x     playing
t+ 63 …  67 s   state 1    static    0.01x     paused
t+ 78 …  90 s   state 1    static    0.00x     paused
t+ 90 … 210 s   state 1    advancing 1.00x     PLAYING, still reporting 1
```

The last thing the bridge sent before t+90 s was `{"playerState":1}`. A play or
pause performed *inside the headset* never reaches the field.

Consequences for anything downstream:

- **Never gate output on `playing`.** It is advisory.
- The authoritative signal is whether position is advancing, and it is
  inherently late: two packets are needed to establish that position stopped,
  and the observed cadence is ~1010 ms, so a headset-initiated pause is
  undetectable for **1.0–2.0 s**. That is a floor, not an estimate.
- Remote-initiated pauses are unaffected — the flag flips because remote-set is
  exactly what it echoes.

`PlayerSnapshot::state_suspect` flags the contradiction when it occurs.

## The HTTP surface now requires a token

`/healthz` and `/ws` refuse a request that does not carry `?t=<token>`. The
pairing URL and QR carry it; `/pair`, `/qr.svg` and the static app do not
require it.

**This is a breaking change for any client that hardcoded `ws://host:8787/ws`,
including the PWA.** The fix is one line — read the token from the URL the
phone was opened with and pass it on:

```js
const token = new URLSearchParams(location.search).get('t')
const ws = new WebSocket(`ws://${location.host}/ws?t=${token}`)
```

Read `src/auth.rs` before assuming this makes anything secure. In particular it
does **not** give confidentiality: the token travels in a URL over plain HTTP
and anyone on the network can read it. It stops one specific, real attack —
a web page you happen to visit opening a WebSocket to your bridge and driving
your player, which no CORS setting prevents. TLS is separate work and neither
substitutes for the other.

The headless binary mints a fresh token each start, so its phone URL changes on
every restart; `--token <hex>` pins one. The desktop app persists its token, so
a home-screen shortcut keeps working.

### Still a hypothesis

- **HereSphere.** The claim that one adapter covers both players rests entirely
  on MFP's two source files being identical in framing. No HereSphere has ever
  been connected to.
- **On-device media.** The observed `path` was an HTTP URL because the media was
  streamed from a DLNA server. A file on the headset presumably reports a
  filesystem path; that form is unobserved.
- **The phone.** The WebSocket relay has only ever been driven from a desktop
  browser.

## Where the framing came from

Two independent sources, which agree:

1. **DeoVR's published remote-control documentation** (<https://deovr.com/app/doc>)
   - "Each packet starts with 4-bytes integer value with length of json data
     represented in UTF8 format."
   - "Remote client also must send a packet (empty or with json) to DeoVR each
     one second for pinging purposes."
   - "If DeoVR won't receive any type of packet for more then 3 seconds it will
     close the connection."
   - Fields: `path`, `duration`, `currentTime`, `playbackSpeed`, `playerState`
     (`Play = 0`, `Pause = 1`).
2. **MultiFunPlayer's `DeoVRMediaSource.cs` and `HereSphereMediaSource.cs`**
   (MIT, © Yoooi). Both read the prefix with `BitConverter.ToInt32`, write it
   with `BitConverter.GetBytes`, and send `new byte[4]` on a 1000 ms timer.
   The two files are identical in framing — which is why one adapter covers
   both players.

Confidence, item by item:

- **4-byte length prefix, UTF-8 JSON payload** — high. Stated outright in the
  DeoVR docs and implemented that way in MFP.
- **Little-endian** — high, but *inferred*. `BitConverter` follows host byte
  order and MFP is a Windows/x86 app; the DeoVR docs do not say. This is the
  one detail that came from inference rather than a spec sentence, so
  `codec.rs` has a test that fails loudly if the byte order is ever flipped.
- **Signed prefix, non-positive means heartbeat** — high. MFP skips
  non-positive lengths, and the docs describe an "empty" packet.
- **1 Hz keepalive, 3 s player-side timeout** — high. Documented and
  implemented consistently.
- **`playerState` 0 = playing, 1 = paused** — high. Documented.
- **HereSphere's `resource` / `identifier` fields** — **low.** These surfaced
  while reading HereSphere's adapter and are treated only as fallbacks for
  media identity. If a real HereSphere never sends them, nothing breaks.

## Running it

Two binaries.

```bash
# Terminal 1 — stand in for a Quest
cargo run --bin fake-player

# Terminal 2 — the bridge
cargo run --bin coyote-bridge -- --player 127.0.0.1:23554 --static-dir ../path/to/pwa/dist
```

Then:

- `http://127.0.0.1:8787/` — the app (or a placeholder if `--static-dir` is unset)
- `http://127.0.0.1:8787/pair` — the QR the phone should scan
- `http://127.0.0.1:8787/healthz` — current state as JSON
- `ws://127.0.0.1:8787/ws` — the state relay
- Tray icon — left-click opens the pairing page

`--help` on either binary lists the rest.

### Against a real Quest

1. **Enable remote control in the player's own settings first.** Neither DeoVR
   nor HereSphere listens on 23554 until that box is ticked — a port scan of a
   running instance finds nothing beforehand. This is the single most likely
   reason for "cannot reach".
2. Find the headset's IP (its Wi-Fi settings). It moves on DHCP; pin a
   reservation if you get tired of retyping it.
3. Start a video playing in the player. Remote control is a player-level
   feature; the DeoVR docs note you have to be inside the video player.
4. `cargo run --bin coyote-bridge -- --player <headset-ip>`
5. Watch the log. Every raw JSON payload is logged at DEBUG:
   `[player] <- {"currentTime":12.5,...}`. **That log is the deliverable of
   this spike** — it is how we find out what a real player actually sends, as
   opposed to what the docs say it sends.

Both machines must be on the same network, and the headset must not be
asleep.

## Seams deliberately left open

Out of scope for the spike, and where each would attach:

- **The signal engine.** `PlayerSnapshot` in `state.rs` is where a funscript
  sampler would read position from. Nothing here touches processing.
- **T-Code LAN listener.** The desktop app's `net.rs` auto-detects T-Code /
  Buttplug / Lovense on one port; that is a separate listener and a separate
  job. `http.rs` copies `net.rs`'s peek-then-route pattern, so adding a third
  branch is the natural extension.
- **TLS.** Everything is plaintext HTTP. **This matters:** Web Bluetooth needs
  a secure context, so the phone will need `https://` before this is usable for
  real. `localhost` is exempt, which is why a desktop browser works today and a
  phone will not. A certificate or a tunnel is required, and neither is here.
- **Auth.** Anything on the LAN can connect and drive the player.
- **Discovery.** The endpoint is typed in, as MFP does.

## Why this is a separate crate

`bridge/` is a standalone cargo package, not a second binary inside
`src-tauri/`. That keeps the spike from touching the desktop app's build at
all, and its dependency list is a strict subset of what `src-tauri` already
compiles (tokio, tokio-tungstenite, futures, serde) plus `qrcode` and the tray
pair — which matters, because the long-term plan is for these two to merge as
the Tauri app sheds its engine, BLE and UI.

Reuse from `src-tauri/src/`:

- `http.rs` copies the peek-then-route approach from `net.rs`, including
  `peek_request_head` almost verbatim. That is what lets one port carry both
  HTTP and WebSocket with no framework.
- `logging.rs` is the desktop ring logger with the Tauri event emit removed —
  same macros, same line format.
- `input_bus.rs` and `tcode_input.rs` were **not** reused: they depend on
  `modulation::AxisState` and the processing state, which is exactly the engine
  the spike does not wire up. They are the right attachment point later.

The tray is built on `tao` + `tray-icon` directly rather than through Tauri.
Those are the crates Tauri 2's own tray is built from, so the eventual move is
a `TrayIconBuilder` swap. Going through Tauri would have meant a second
`tauri.conf.json`, icon set and frontend build for no extra capability.

Tray support is behind a default-on `tray` feature; `--no-default-features`
drops the windowing stack entirely, for the headless home server the spike
record says this will actually live on.
