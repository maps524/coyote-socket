# Media Sources — What MFP Does, and What a Browser Can Do

Derived from reading `MultiFunPlayer/Source/MultiFunPlayer/MediaSource/ViewModels/*.cs`
(MIT licensed, © 2020 Yoooi — portable with attribution).

## The whole matrix

| Player | MFP transport | Detail | Browser-reachable? | Needs bridge? |
|---|---|---|---|---|
| **Internal player** | in-process | MFP plays scripts with no video | ✅ trivially — use `<video>` | no |
| **OpenFunscripter** | `ws://127.0.0.1:8080/ofs` | JSON over **WebSocket** | ⚠️ only blocker is `wss` / mixed content | maybe not |
| **Jellyfin** | HTTP `:8096` `/Sessions?api_key=` | REST + API key | ⚠️ CORS-friendly server, HTTPS-capable | no, if HTTPS |
| **Emby** | HTTP `:8096` `/Sessions` | REST + API key | ⚠️ same | no, if HTTPS |
| **Plex** | HTTP `:32400` `/player/timeline/poll` | REST + token | ⚠️ same; Plex already ships `*.plex.direct` certs | no |
| **VLC** | HTTP `:8080` `/requests/status.json` | polled JSON, HTTP Basic auth | ❌ no CORS headers, no HTTPS | **yes** |
| **MPC-HC/BE** | HTTP `:13579` `/variables.html` | scraped HTML | ❌ no CORS headers | **yes** |
| **DeoVR** | **raw TCP** `:23554` | 4-byte LE length prefix + UTF-8 JSON, 1 Hz keepalive, 3 s timeout | ❌ raw socket | **yes** |
| **HereSphere** | **raw TCP** `:23554` | same framing as DeoVR | ❌ raw socket | **yes** |
| **Whirligig** | **raw TCP** `:2000` | line protocol | ❌ raw socket | **yes** |
| **MPV** | **named pipe** `multifunplayer-mpv` | JSON IPC; MFP even launches mpv itself | ❌ no IPC from a browser | **yes** |
| **PotPlayer** | **Win32 window messages** | `Process` handle + `SendMessage` | ❌ | **yes** |

## Reading the matrix

**Three genuinely browser-native options exist today:**

1. **Internal player** — the strategic one. If the video plays inside the PWA, there is no sync
   problem at all: `video.currentTime` *is* the truth. This is the path to "one app, one device."
2. **OpenFunscripter** — already WebSocket, already JSON. The only obstacle is that OFS listens on
   `ws://` and an `https://` page can't connect to it. If OFS ever supports `wss` (or you accept a
   bridge), it's free.
3. **Jellyfin / Emby / Plex** — real web servers, already serving browser clients, already capable of
   HTTPS with valid certs. Plex in particular solved the exact LAN-HTTPS problem with `*.plex.direct`,
   which is why that pattern is recommended for the bridge in `02-architecture.md`.

**Everything else needs a bridge, and no browser technology changes that.** WASM does not grant
socket access; it runs inside the same sandbox as JavaScript. There is no WASM escape hatch for
DeoVR's TCP socket or mpv's named pipe. WebTransport and WebRTC data channels require a cooperating
peer that speaks those protocols — VLC does not.

> **Update 2026-07-28:** DeoVR and HereSphere are settled — both are raw TCP on 23554 with identical
> framing, both need remote control enabled in their settings first, and one bridge adapter covers
> both. HereSphere's port-5000 "Web API" is a library feed it consumes as a client, not a control
> surface. See `09-quest-and-players.md`.

## Why VLC needs a bridge when it looks like "just a URL"

This is the most common objection, and it's a fair one. VLC's status endpoint is an ordinary
`GET http://192.168.1.50:8080/requests/status.json` returning JSON. MFP fetches it with a plain
HTTP client and it works.

The browser is not less capable — it applies two extra checks a native process doesn't:

1. **Mixed content.** An `https://` page may not make a plain `http://` request. Blocked before it
   leaves the tab.
2. **CORS.** Even over HTTPS, the browser will not hand your JavaScript the response body unless the
   *server* returns `Access-Control-Allow-Origin` naming your origin. You cannot opt out of this from
   the client side — no fetch option, no flag, no WASM trick. Only the server can grant it.

### Measured, not inferred (2026-07-27, VLC running locally)

```
$ curl -i -H "Origin: https://example.com" http://127.0.0.1:8080/requests/status.json
HTTP/1.0 401 Unauthorized
WWW-Authenticate: Basic realm="VLC stream"
Content-Length: 338
Content-Type: text/html
```

```
$ curl -i -X OPTIONS -H "Origin: https://example.com" \
       -H "Access-Control-Request-Method: GET" \
       -H "Access-Control-Request-Headers: authorization" \
       http://127.0.0.1:8080/requests/status.json
HTTP/1.0 501 Not implemented
```

Two independent failures:

- **No `Access-Control-*` headers at all.** The `Origin` request header is ignored.
- **`OPTIONS` returns 501.** This is the decisive one. MFP sends `Authorization: Basic …`, which is a
  non-simple header, so a browser *must* preflight with `OPTIONS` before the `GET`. VLC answers 501,
  so the real request is never issued.

Client-side workarounds and why each fails:

| Attempt | Result |
|---|---|
| `mode: 'no-cors'` | Opaque response. Body unreadable. Useless for JSON. |
| Drop the password (simple GET, no preflight) | Preflight avoided, but the response still carries no `ACAO`, so the browser withholds the body. |
| Local Network Access + `targetAddressSpace` | Fixes *mixed content only*. Its private-network preflight is also an `OPTIONS` → same 501. |

Note the response is `HTTP/1.0`. This interface predates CORS by roughly a decade.

**Any reverse proxy that adds `Access-Control-Allow-Origin` and answers `OPTIONS` fixes VLC.** Three
lines of Caddy or nginx would do it. The bridge is that proxy, plus the raw-socket players, plus a
normalized message shape.

**So the bridge is a permission gap, not a capability gap.** It exists to say "yes, this origin may
read me." And once it exists, it earns its keep twice, because the same process is the only thing
that can hold a raw TCP socket to DeoVR or a named pipe to mpv.

### Where the bridge should run

Not necessarily on the machine playing the video. A **single instance on a home server**, next to the
media library, is the better deployment: one thing to install and update, reachable from every device
on the LAN, and it doubles as the entry point into the home network for anything else the app wants
to reach.

Reaching a player on a *different* network (a hotel TV, a friend's PC) is a separate problem with
existing answers — a mesh VPN like Tailscale, or a reverse proxy — and should not shape this design.
The honest fallback for away-from-home use is Tier A: the video plays in the PWA, and nothing needs
to be reached at all.

## The nasty detail: mixed content

Even for the HTTP players, an `https://` PWA is blocked from `http://192.168.x.x` before CORS is
even considered. Chrome's Local Network Access permission can waive the mixed-content check when the
target is a private IP literal, a `.local` name, or a `fetch()` annotated with
`targetAddressSpace: "local"` — but the target must then answer the private-network preflight with
`Access-Control-Allow-Private-Network: true`. VLC and MPC-HC will never send that header.

So the practical ordering is:

- server speaks HTTPS with a trusted cert → works
- server is yours (the bridge) → make it speak HTTPS, works
- server is VLC → bridge

## What the bridge must normalize

MFP's message set, which every source maps onto. Copy it:

| Message | Meaning |
|---|---|
| `MediaPathChanged` | new file loaded → look up scripts |
| `MediaPositionChanged` | position, with a `forceSeek` flag |
| `MediaPlayPause` | playing / paused |
| `MediaDurationChanged` | total length |
| `MediaSpeedChanged` | playback rate (scales script time) |

Note the direction is bidirectional in MFP: it can also *drive* the player (seek, play/pause, open
path). Keep that in the bridge protocol — controlling the video from your phone is a large part of
the "one device" experience.

## Path matching

MFP has `MediaPathModifier`s (find/replace, URL-decode) because the path the player reports rarely
matches the path where scripts live — especially across a network share. Any bridge or repository
design needs the same escape hatch. In a PWA, a **content hash or a stable media ID** is a better
primary key than a filesystem path; fall back to filename matching.

## Recommendation

Build the internal player first and treat it as the flagship experience, not a fallback. It is the
only configuration where the entire stack is browser-native, and it is the configuration that
actually achieves the stated goal of "just my phone."

Ship bridge support second, and scope the bridge to a handful of players (VLC, MPV, DeoVR,
HereSphere) rather than all twelve — the plugin system covers the long tail.
