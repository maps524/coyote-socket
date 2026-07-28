# Roadmap, Effort, and Risk

Estimates are in **focused developer-days** for one person who already knows this codebase. They
assume the existing Rust signal engine is reused rather than rewritten. Multiply by whatever your
personal calendar-time factor is.

---

## Phase 0 — De-risking spike ✅ DONE (2026-07-28)

**Complete. Every question came back yes — including on iOS, which the rest of this plan was
written assuming was out.** Results in `07-measured-results.md`; the probe is in `probe/`.

Summary: Coyote connects from an iOS browser in ~900 ms with `writeWithoutResponse`; analog
triggers work; Wake Lock is granted; the 10 Hz loop is stable and survived backgrounding with a
verified-live radio link. Adjust the estimates below accordingly — the platform risk that drove
them is now retired.

The original plan, for reference:

| Spike | Question | Days |
|---|---|---|
| BLE proof | Can a plain page connect to the Coyote 3.0 and hold a stable 10 Hz B0 write for 30 min? | 1 |
| Throttle proof | With Screen Wake Lock + Worker tick, what actually happens on screen-off, app-switch, and incoming call? | 1 |
| Gamepad proof | Does an OS-paired BT controller show up in Chrome Android's Gamepad API, with usable latency? | 0.5 |
| WASM proof | Does `modulation.rs` + `protocol.rs` build for `wasm32` and produce byte-identical B0 output vs. the desktop app for a fixed input trace? | 1.5 |
| Mixed-content proof | Can an `https://` page reach a local WebSocket at all — LNA permission, or does it force the cert route? | 1 |

**Exit criteria:** a page that connects to the Coyote, runs a funscript, and doesn't stutter.

---

## Phase 1 — Tier A: the self-contained PWA (15–25 days)

The product that removes MFP *and* the video player.

- Extract `crates/coyote-core` from `src-tauri`, add `coyote-wasm` bindings, wire into CI **(4–6 d)**
- Worker tick loop + BLE writer + safety/deadman layer **(3–4 d)**
- Funscript parse, pchip interpolation, heatmap canvas **(2–3 d)**
- Internal `<video>` player + local file import (video + script) + OPFS caching **(2–3 d)**
- Port the channel/parameter/preset UI from the existing Svelte 5 components **(3–5 d)**
- Gamepad input + binding UI **(2 d)**
- PWA shell: manifest, service worker, install flow, settings in IndexedDB **(1–2 d)**

**Ship here.** This is a genuinely useful product on its own, and the entire "just my phone" story.

---

## Phase 2 — Tier B: networked libraries (8–12 days)

- `ScriptRepository` interface + static-index reference implementation **(2 d)**
- Stash and XBVR repositories **(3–4 d)**
- `SyncSource` interface + Jellyfin/Plex HTTP pollers **(3–4 d)**
- Media→script matching, path modifiers, hash-based IDs **(1–2 d)**

---

## Phase 3 — Tier C: `coyote-bridge` (12–20 days)

- Bridge skeleton: WebSocket server, normalized message set, config **(3 d)**
- TLS story — whichever Phase 0 chose (cert distribution, DNS, renewal is ongoing work) **(3–5 d)**
- Player adapters, ported from MFP: VLC, MPV, DeoVR, HereSphere **(4–6 d)**
- `BridgeSync` client in the PWA, pairing/discovery UX **(2–3 d)**
- Packaging for Windows/Linux/macOS, autostart **(2–3 d)**

---

## Phase 4 — Plugins and open-source launch (10–15 days)

- Worker sandbox, host API, SRI loading, permission prompts **(4–6 d)**
- Settings-schema renderer **(2 d)**
- Registry format, official registry, one reference plugin per extension point **(2–3 d)**
- Docs: contributor guide, protocol docs, plugin authoring guide **(2–4 d)**

---

## Total

Roughly **50–75 focused days** for the whole thing; **20–30** to a shippable Phase 1. The single
biggest lever on that number is the core-extraction refactor — do it well and Phases 1–4 all get
cheaper, do it badly and you maintain two divergent signal engines forever.

---

## What will go well

- **The signal engine transfers cleanly.** ~4,300 lines of `protocol` / `waveform` / `modulation` /
  `resolver` / `transforms` have no platform dependencies beyond `serde`. This is the hard,
  safety-critical, well-tested part of the app and it comes along for free.
- **Funscript is a trivial format.** JSON, monotonic timestamps, 0–100 positions.
- **The input abstraction already exists.** `ParameterSource` linking a T-Code axis is exactly the
  shape a funscript needs. Playback really is "just a new input format."
- **BLE bandwidth is a non-issue.** 20 bytes at 10 Hz against a link that does ~90 kB/s.
- **The risky bits are now measured, not assumed.** The probe drives this exact Coyote from a phone
  browser: ~900 ms connect, `writeWithoutResponse`, analog triggers, stable 10 Hz loop.
- **Distribution gets dramatically better.** A URL instead of a signed Windows installer.

## What will be painful

| Risk | Severity | Mitigation |
|---|---|---|
| ~~Background throttling kills the tick loop~~ | **Retired** | Measured: loop stable foreground, and survived backgrounding with a live link. Still ship Wake Lock + hard-zero deadman — background time is revocable. |
| **`https://` PWA can't reach LAN players** | High | Bridge with a real cert (Plex model), or a tunnel — the Phase 0 spike used a Cloudflare quick tunnel successfully. LNA for WebSocket still unproven. |
| **Raw-TCP players are permanently unreachable** | High | Accept it. Bridge or nothing — say so in the README so contributors don't chase it. |
| ~~No iOS~~ | **Retired** | Measured working via Bluefy on iOS 18.7. Capacitor is now a nicer-install option, not a prerequisite. |
| **Web Bluetooth reconnect UX** | Medium | Still unmeasured. `getDevices()` + `watchAdvertisements`; worst case, one tap per session. |
| **Two BLE code paths to maintain** | Medium | Keep them thin: the core is shared, only the transport differs. Consider retiring the Tauri app once the PWA matures. |
| **Safety regressions from a less reliable runtime** | High | Clamps in WASM, deadman on the writer, ramp-to-zero on any doubt. This deserves its own test plan and a written safety review. |
| **Plugin supply chain** | Medium | SRI, permissions, Worker isolation, curated registry. |
| **No local directory access on Android** | Low | Import-and-cache into OPFS; remote repositories for libraries. |
| **Chromium + Bluefy only** | Low | Chrome, Edge, Opera, Brave, Samsung Internet, and Bluefy on iOS. Desktop Safari and Firefox are out; say so plainly. |

## Strategic note

There are two products hiding in this idea, and it's worth being deliberate about which one you're
building:

1. **"CoyoteSocket, but on my phone"** — Tier A. Small, self-contained, achievable in a month, and it
   already collapses three apps into one for the common case.
2. **"An open, pluggable, browser-native replacement for the whole MFP stack"** — Tiers B+C+plugins.
   Much bigger, needs a bridge binary anyway, and its ceiling is set by how many contributors show
   up.

Phase 1 stands alone and doesn't foreclose Phase 3. Build it first, use it for a few weeks, and let
real usage decide whether the bridge is worth it.
