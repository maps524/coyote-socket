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

## The funscript library

The bridge runs on the machine where the media lives, already serves the PWA,
and already knows what the player is playing. So it serves the scripts too:

```
GET /library/index.json   -> { "scripts": [ { "name", "bytes", "modifiedMs" } ],
                              "configured", "scan", "scannedAtMs", "ageMs",
                              "checkedAtMs", "generation" }
GET /library/<name>       -> the funscript bytes
```

Both are token-gated, like `/healthz` and `/ws`. Point the bridge at a folder
with `--library-dir <path>` (or `libraryDir` in the app's settings file). **No
library configured is a normal state**, not an error: the index answers 200 with
an empty list and `configured: false`, so the app can say "you have not pointed
me at a folder" rather than showing a failure.

`configured` and `scan` answer different questions and a UI needs both.
`configured` is whether a path is set; `scan` is `"pending"`, `"ok"` or
`"failed"` for whether the last attempt to read it worked. **A failed scan keeps
the previous listing** rather than replacing it with an empty one — otherwise a
network share dropping for a single poll tick tells every phone the library is
empty. `scannedAtMs` / `ageMs` describe the listing, and are `null` before the
first successful scan; `checkedAtMs` describes the last attempt. When those
diverge, something is wrong and the gap is how far behind you are.

Readability is checked every ~2 s by opening the directory, not by looking at
its timestamp — a share that is still there but has stopped answering leaves the
timestamp alone, and a `scan: "ok"` with a `checkedAtMs` that quietly stopped
advancing is the one failure this API must not have.

A `library` WebSocket message — `{"type":"library","generation":N,"count":M,
"scan":S,"scannedAtMs":T}` — means **re-fetch the index**. One is sent on
connect, whether or not a library is configured, and one each time the contents
or the scan state change, so a phone already connected picks up a new file
without a reload. It deliberately carries no listing: contents travel over the
request/response that stamps its own freshness, not over the relay's `watch`
channel — which, as `ws_relay`'s contract records, collapses an unbounded number
of updates into one delivery for a slow consumer.

Two contracts worth knowing before writing a client:

- **Names round-trip exactly** — no case folding, no Unicode normalisation. Ask
  for the name the index gave you. (`naming.ts` case-folds when matching a
  script to media, which is right there; the folded name is not the fetch key.)
- **Symlinks and junctions inside the folder are followed**, so a library
  assembled out of links into several drives works.

A new file appears within ~2 s, not 60: dropping one moves the directory's
mtime. The 60 s full rescan only bounds how stale `bytes` and `modifiedMs` can
get for a file edited in place — except on an SMB share, where cached directory
metadata can delay the mtime change and 60 s becomes the worst case for noticing
a new file at all.

**The bridge does not match scripts to media.** That lives in the client, in
`src/lib/script/naming.ts` in `coyote-socket-web` — the MultiFunPlayer suffix
convention, DLNA URLs, and a documented tie-break, already tested and merged. Two
implementations of a naming convention diverge, and the divergence shows up as
"the script I can see will not load". The bridge serves names; the client
matches them against the `path` it gets in every snapshot.

Details — path handling, the freshness contract, and what a 10,000-file
directory costs — are in `src/library.rs`'s module documentation.
## Who is connected

`/healthz` carries a `clients` object beside the player snapshot — every
existing key is where it was, so a consumer reading `positionS` off the top
level is unaffected. The desktop window renders the same thing in its Clients
panel. `src/clients.rs` is the whole of it.

```jsonc
"clients": {
  "connections": 2,                 // open sockets — measured, not inferred
  "browsers": { "state": "atLeast", "count": 1, "unidentified": 1 },
  "anyUnidentified": true,
  "credentialsAvailable": false,
  "clients": [ /* one row per identity, connected first */ ]
}
```

**Read `browsers` carefully — it is the point of the feature.** A phone that
roams between access points drops its socket and opens a new one, and *nothing
the bridge can observe on its own tells that apart from a second phone*. The
address is not identity (NAT, DHCP, a fresh ephemeral port every time), and the
pairing token is shared by every device by design. So:

- `{"state": "reported", "count": n}` — every connected client presented an id,
  and `n` is how many distinct ones. Still not certainty: a browser that cleared
  its storage counts as new.
- `{"state": "atLeast", …}` — at least one connection presented nothing, so
  `count` is a **floor**. The window renders a range and the reason, never a
  number. A count that silently merges a roam, or splits one, is worse than no
  count.

Identity comes in two grades, on `provenance`:

| `provenance` | What it is | Worth |
|---|---|---|
| `credential` | A per-device credential the bridge verified. | Survives a roam, a restart and someone trying to forge it. |
| `selfReported` | `?c=<id>` on `/ws`, or `{"type":"hello","clientId":…,"label":…}`. | Better than nothing; anyone with the token can send any id. |
| absent | Nothing presented. | The connection stands alone and is never merged with anything. |

The two never merge — the provenance is part of the grouping key, so a client
cannot claim its way onto someone else's credentialed row.

**Even a credential identifies a browser storage partition, not a handset and
not a person.** Safari and a home-screen install on one phone may hold two and
show as two. That is why the field says `browsers`.

Revocation is split deliberately: the credential store deletes the credential
(so it cannot be presented again), then calls `ClientRegistry::revoke(id)`,
which closes that device's **live** sockets with close code `4001` and reason
`revoked`. Both halves must fire. Closing without deleting lets the device
reconnect; deleting without closing leaves a revoked phone driving hardware
until its socket happens to drop.

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
cargo run --bin coyote-bridge -- --player 127.0.0.1:23554 \
    --static-dir ../path/to/pwa/dist \
    --library-dir /path/to/funscripts
```

Then:

- `http://127.0.0.1:8787/` — the app (or a placeholder if `--static-dir` is unset)
- `http://127.0.0.1:8787/pair` — the QR the phone should scan
- `http://127.0.0.1:8787/install` — how the phone gets a secure context
- `http://127.0.0.1:8787/healthz` — current state as JSON
- `http://127.0.0.1:8787/library/index.json` — the funscript listing
- `wss://coyote.local:8443/ws` — the state relay (a page served over HTTPS
  cannot open a `ws://` socket, so this is the address the app actually uses)
- `http://127.0.0.1:8787/dlna/index.json` — media servers on the network
- Tray icon — left-click opens the pairing page

`--help` on either binary lists the rest.

## The phone needs HTTPS, and the bridge issues its own certificate

**Web Bluetooth requires a secure context.** `localhost` is exempt, which is
exactly what hides this during desktop testing — the phone is not localhost.
Over plain HTTP the phone cannot reach the Coyote at all, and over a tunnel it
cannot open a socket back to a LAN service. Neither arrangement completes the
chain, so the bridge runs its own certificate authority.

On first run it generates a CA, keeps the private key in `<config>/tls/`, and
serves the public certificate from `/install`. The phone installs it once.

- **The address is `coyote.local`**, answered by an mDNS responder inside the
  bridge, and it is what the QR carries. It has to be the bridge's own
  responder: Windows' built-in one was measured advertising a virtual adapter
  (`172.21.160.1`, WSL's switch) rather than the LAN address, which a phone
  cannot reach — and the resulting failure would have looked like a certificate
  problem, because every visible symptom points there. **That responder is why
  this works at all**; it is not an optimisation to drop in favour of the OS.
  The current IP is in the certificate too, and is used on the QR only when
  mDNS could not start.
- **A stable name is the point.** The origin is what OPFS, the PWA install and
  the Web Bluetooth device grant are keyed on. A tunnel that mints a new
  hostname every restart resets all three — that is the difference between an
  app and a demo.
- **The QR points at plain HTTP**, deliberately. A phone that has not yet
  trusted the CA meets a full-page certificate interstitial on HTTPS with no
  route back to the instructions.
- **Both listeners stay up.** Everything except Web Bluetooth works over plain
  HTTP and it is far easier to debug.

### Pair once, then never present the token again

The QR carries the pairing token. It is used **exactly once**, at
`/pair/exchange` on the HTTPS origin, which trades it for a per-device
credential in a cookie and redirects so the token leaves the address bar.
Everything afterwards — the app, `/healthz`, the WebSocket — authorises on that
cookie.

**Why the exchange has to happen there.** Pairing starts on
`http://coyote.local:8787` and the app runs on `https://coyote.local:8443`.
Different scheme *and* different port means different origin, so **nothing in
browser storage crosses** — no `localStorage`, no cookies, nothing. An earlier
design expected the token to survive that jump and it could not: the socket was
refused, which a browser reports as close code 1006 with no reason, which the
app could only render as "bridge unreachable". The origin change is the point of
the flow and it is also what loses the token, so the token crosses on the URL,
once, into the endpoint built to catch it.

**Why a cookie rather than `localStorage`.** Cookies ride the WebSocket upgrade
automatically. `localStorage` does not, so every socket would need JavaScript to
read the value and append it — a second code path that can be wrong, in the one
place where being wrong looks like a dead network. `HttpOnly` then comes free,
and it is most of the value: no script can read the credential.

**Why per-device.** One shared token makes revocation all-or-nothing: rotate it
and every paired device stops, which is why auto-rotation was rejected — pairing
a tablet would silently un-pair the phone. Per-device credentials make revoke
mean *"stop trusting the tablet"*. Secrets are stored hashed, so the credential
file cannot be replayed as a cookie if it ends up in a bug report.

A credential identifies **a browser storage partition**, not a handset and not a
person. Two browsers on one phone are two devices here; a phone that clears its
cookies is a new one.

### You will need two browsers on iOS, and that is not a bug

- **Install the certificate in Safari.** It is the only iOS browser that offers
  to install a configuration profile.
- **Open the app in Bluefy.** **Safari does not implement Web Bluetooth and
  never has** — a settled limitation, established by this project's capability
  probe, and the reason Bluefy is in the plan at all. Opening the app in Safari
  gives a page that loads and simply never finds the Coyote.

The certificate is installed into the iOS system trust store, and **Bluefy does
inherit it** — verified on a real iPhone on 2026-07-29, with the Coyote
connected over Bluetooth from `https://coyote.local:8443`. One handset, one iOS
version, one Bluefy version, once; enough to build on, not enough to call
universal.

### The failure to check first looks like a network problem

Browsing to an untrusted HTTPS address shows an interstitial you can read. The
app does not browse — it opens a `wss://` socket, and **an untrusted socket in a
WKWebView is refused silently**: no interstitial, no error text, no mention of
certificates. It surfaces as close code 1006, which the app reports as "bridge
unreachable", which sends the user to check their Wi-Fi.

Same certificate, same host, two completely different-looking failures. That is
why `/secure-check` exists: **if that page loads, the certificate is trusted and
the network is fine**, so a failing socket is about trust and not about the LAN.

### On iOS there are two steps, and the second is the one that gets missed

Installing the profile does **not** trust it. The certificate is inert until
Settings → General → About → **Certificate Trust Settings** → switch on the
root. Skipping it fails indistinguishably from a broken certificate.

The install page carries the numbered path and a **"check my trust" button**
that probes plain HTTP first and TLS second, so "your network is blocking mDNS"
and "you missed the trust step" — the same symptom otherwise — read differently.

### What this does not give you

**TLS provides confidentiality and a secure context. It provides no
authorization.** Once the CA is installed, every device on the LAN handshakes
exactly as successfully as the phone. Authorization is the pairing token, which
is a different mechanism answering a different attacker — and the token travels
in cleartext on the first hop, so neither half makes the other redundant.
Neither, alone or together, makes "the bridge is secure" a true sentence.

### The CA is yours and it stays on your machine

No CA key is shipped in any binary. One is generated per install, on the user's
own machine, and the private key is never sent anywhere — not in the QR, not
over the network, not into a log. Installing a root does let it vouch for any
domain, so the install page says so rather than burying it, names the
certificate so it can be found months later, and documents removal.

Deleting `<config>/tls/` means every phone has to install a new certificate.

## DLNA browsing and the media proxy

The phone cannot discover anything on the network — SSDP needs UDP multicast
and JavaScript has no UDP primitive, permanently. And it cannot play from a
media server directly either: the app is on HTTPS because Web Bluetooth
requires a secure context, and an HTTPS page cannot load an `http://` video.

Both problems land on the bridge. Three endpoints, token-gated exactly like
`/healthz`, sharing `/library`'s JSON envelope:

```text
GET /dlna/index.json[?refresh=1]                    media servers, and why the list is what it is
GET /dlna/browse.json?server=<udn>&object=<id>      one directory level
GET /dlna/media/<ref>                               the bytes, range-correct
```

### What is proven and what is not

| Part | Status |
|---|---|
| Range handling — `206`, `Content-Range`, `If-Range`, `bytes=N-`, suffix, `416`, `HEAD` | **Tested against a real socket** through the real router and proxy, in `tests/dlna_media.rs`. |
| Streaming rather than buffering | **Tested** — the response head must arrive before the upstream body is complete. |
| Compensating for a server that ignores `Range` | **Tested** against a fake server that does exactly that. |
| Refusing a `<res>` on another host | **Tested.** |
| DIDL and device-description parsing | **Tested** against captured-shape documents, including truncated and malformed input. |
| SSDP `M-SEARCH` against a real network | **Run, once, by hand.** Found `MPVR-UMS` (UMS 15.7.0, Linux) at `192.168.0.4:5001` in 2.5 s: 3 searches sent from `192.168.0.9`, 3 replies. No test multicasts. |
| Browsing a real Universal Media Server | **Run by hand.** Root, then a 48-entry `videos` folder, with real `dc:title`, `duration`, `resolution` and `size`. |
| Range requests against a real UMS | **Run by hand**, and byte-for-byte verified — see below. |
| Playing in Safari or Bluefy | **Never tried.** No phone has loaded any of this. |
| HereSphere, on-device media | Out of scope here, and unobserved as ever. |

Scope that precisely: the transport is exercised and one real media server has
been browsed and streamed from. Nothing has been played in a browser.

### `<res>` selection is not "take the first one"

A server advertises the same item several times — the original, a transcode,
sometimes a stream over a protocol a browser cannot open at all. Taking the
first gives a video element that loads and shows nothing, which is a silent
failure that gets blamed on the file. `upnp::choose_res` ranks by byte-range
support (`DLNA.ORG_OP`'s **second** digit), then original over transcode, then
container; and it carries the ones it rejected, with reasons, into
`/dlna/browse.json`. When something will not play, what else was on offer is
one field away rather than a packet capture away.

`seekable: false` in a listing means the *server* said it does not honour byte
ranges. Scrubbing will not work and the proxy is not the reason.

### Verified against a real media server

UMS 15.7.0, a 2,097,190,731-byte video, bridge and server on **different**
machines. 64 KiB fetched through the proxy and straight from UMS, at four
offsets including the last block: **byte-identical every time.**

```text
bytes=0-65535                    IDENTICAL
bytes=1000000000-1000065535      IDENTICAL
bytes=2000000000-2000065535      IDENTICAL
bytes=2097125195-2097190730      IDENTICAL   (the final block)
```

An open-ended seek 2 GB in answered `206 … Content-Range: bytes
2000000000-2097190730/2097190731` with **15 ms** to first byte.

### The cost of being in the playback path

**Measured, on the worse of the two deployments.** 200 MB pulled through the
proxy from a media server on another machine, debug build:

| | Throughput | Time |
|---|---|---|
| Straight from UMS | 107 MB/s | 1.96 s |
| Through the bridge | 55 MB/s | 3.82 s |

**Almost exactly half**, which is the arithmetic of the bytes crossing the
network twice rather than any cost in the proxy itself. It is also still about
eighteen times a 25 Mb/s VR stream, so on a wired or decent wireless link this
is not the constraint. On a congested 2.4 GHz network, where both hops share one
radio, it is the difference between playing and stalling.

**Co-location removes the second hop entirely** — that is the deployment this
was designed for, and the case where the proxy costs a memcpy through a 64 KiB
buffer. It has not been measured, because the machine to measure it on is the
one running UMS.

There is no way to avoid any of this while the page is on HTTPS, which it must
be for Web Bluetooth.

### When discovery finds nothing

An empty list is three different situations and they are reported as three
different messages, because "no media servers found" otherwise reads as a
statement about the network when the search may never have left the machine:

- **Nothing answered at all** — including devices that are not media servers.
  That points at the search not reaching the network. On a Windows box the
  usual cause is the datagram leaving via a WSL or Hyper-V adapter.
- **Things answered, none was a media server** — the search works. If UMS is
  running, check its **IP allowlist** includes this host; that is a component
  we do not own and it fails silently.
- **A server answered but could not be described** — reported separately, with
  the HTTP status, because it is not an empty network.

`/dlna/index.json` carries the evidence for whichever it is: where the search
was sent from, how many went out, and how many replies of any kind came back.

**`--dlna-server http://192.168.0.4:5001/description/fetch`** pins a server by
address and skips discovery entirely, for a network where multicast cannot
work. Repeatable. A bad address is reported at startup rather than as an empty
library later.

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
