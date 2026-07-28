# Quest Browser, DLNA, and the Player APIs

Scouted 2026-07-28, after the question came up: could the whole app live *inside* the headset's
browser, so nothing connects to a phone at all?

## Verdict: no. Web Bluetooth is unavailable in the Meta Quest Browser.

Everything else the app needs works there. That one thing doesn't, and it's fatal.

| Capability | Quest Browser |
|---|---|
| **Web Bluetooth** | ❌ **unavailable** |
| WebXR (`immersive-vr`, `immersive-ar`, hand tracking) | ✅ |
| Controller input, analog triggers | ✅ via `XRInputSource.gamepad`, `buttons[i].value` |
| `navigator.getGamepads()` outside an XR session | ⚠️ unconfirmed |
| Video playback, incl. 180/360 stereo | ✅ standard `<video>` + WebXR sphere |
| Local file picker onto headset storage | ⚠️ likely not |
| Local network (mixed content / PNA) | ✅ standard Chromium rules |

Multiple developer reports — most recent July 2025 — describe `navigator.bluetooth` behaving as
though the headset has no BLE radio at all. The same code works in a stock Chromium build
sideloaded onto the same hardware, which points at Meta gating the feature rather than an engine
limitation. No official Meta documentation lists Web Bluetooth as supported, and no flag or
workaround was found. The complaint has been open for over a year.

Consequence: a Quest-hosted version would still need a separate process holding the BLE
connection — the phone, or a native bridge. It would add complexity and remove nothing.

Sources:
[Meta forum: Web Bluetooth in Quest Browser](https://communityforums.atmeta.com/discussions/dev-quest/support-of-web-bluetooth-ble-in-quest-browser/1242380),
[Meta forum: web bluetooth / WebXR](https://communityforums.atmeta.com/t5/Quest-Development/web-bluetooth-Web-XR/td-p/866926),
[Meta WebXR overview](https://developers.meta.com/horizon/documentation/web/webxr-overview/),
[Meta browser specs](https://developers.meta.com/horizon/documentation/web/browser-specs/)

---

## DLNA / UPnP: permanently impossible from any browser

Not a Quest quirk and not a policy choice — the capability does not exist in the web platform.

SSDP discovery requires sending UDP multicast (`M-SEARCH` to `239.255.255.250:1900`) and receiving
unicast UDP replies. JavaScript has no UDP socket primitive; WebRTC's UDP usage is confined to ICE
and data channels, not arbitrary multicast. No browser on any device can discover or negotiate
DLNA streaming from a page.

Close this line of thinking off for good.

---

## Player remote APIs — both need the bridge

### DeoVR

Raw TCP on port **23554**. 4-byte length-prefixed UTF-8 JSON, 1 Hz keepalive, 3 s timeout.
Confirmed by reading MFP's `DeoVRMediaSource.cs`. Not HTTP, not reachable from a browser.

### HereSphere

Raw TCP on port **23554**, same framing. Confirmed in MFP's `HereSphereMediaSource.cs`.

**Correction worth recording.** An earlier note in this spike suggested HereSphere exposes an
HTTP/JSON API on port 5000 that a browser could reach directly, which would have eliminated the
bridge. That was wrong, and the direction is backwards: port 5000 belongs to a **library server
that HereSphere connects out to as an HTTP client**. From the `heresphere-server` README:

> "Load the HereSphere web browser and click the cog wheel to open the settings. In the
> `Link Server` field, enter the URL you were given from the server window."

That is a media library feed. It carries no playback position and is not a control surface.

Both players also require remote control to be **explicitly enabled in their settings** before
anything listens on 23554 — a scan of a running HereSphere instance found no open ports until then.

### The upside

Both players use **the same framing on the same port**. One bridge adapter covers both. You are not
writing a plugin per player.

---

## Resulting topology

```
Quest (HereSphere or DeoVR, TCP :23554)
   ▲
   │ raw TCP — the bridge dials in
   │
Bridge (home server, small Rust binary)
   │
   │ wss:// — the phone dials in
   ▼
Phone (PWA)  ──Bluetooth──▶  Coyote
```

Three requirements:

1. Bridge must be able to reach the headset's IP
2. Phone must be able to reach the bridge over `wss://` — needs a trusted certificate or a tunnel
3. Phone to Coyote is Bluetooth, independent of all networking

No PC in the room. One always-on process on the box the media library already lives on.

Discovery is the fiddly part: the headset's IP moves on DHCP. Either scan for a listener on 23554
or pin a reservation. MFP simply has the user type the endpoint, which is a fine starting point.
