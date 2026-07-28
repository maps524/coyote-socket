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

| Part | Status |
|---|---|
| Length-prefixed framing | Unit-tested. Format taken from two agreeing sources (below). |
| Keepalive, timeout, reconnect | Tested against `fake-player`, which enforces the real 3 s timeout. |
| Static file serving | Tested, and run by hand against a directory of files. |
| WebSocket relay | Tested, and driven by hand from a browser `WebSocket`. |
| Seek from the phone | Works end to end against `fake-player`. |
| **Talking to a real DeoVR or HereSphere** | **Never done. Nobody has run this against a headset.** |

That last row is the whole point of the caveat. The client has only ever spoken
to `fake-player`, which was written from the same reading of the same two
sources. A shared misreading passes every test in this repo. Until someone runs
it against a Quest, **the protocol client is a hypothesis.**

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
