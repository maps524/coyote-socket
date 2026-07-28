# V1 Scope

Captured from a working session on 2026-07-28, after Phase 0 came back clean. This is the
build spec — where `05-roadmap.md` disagrees, this wins.

## The actual goal

**VR headset playing the video, phone in hand as the controller, no PC in the room.** That's the
end state. Today it takes MultiFunPlayer, CoyoteSocket and a desktop video player.

V1 does not reach that end state, and that's deliberate — see below.

---

## V1: internal player

The PWA plays the video itself, samples the funscript, and drives the Coyote over Web Bluetooth.
No bridge, no server, nothing else running.

**Why start here when the goal is DeoVR on a Quest.** DeoVR's remote-control protocol is raw TCP
on port 23554 (length-prefixed JSON), which a browser cannot open — confirmed by reading MFP's
source. Reaching it needs a bridge process somewhere on the network. Everything *else* in the app
— the WASM engine, funscript sampling, gamepad handling, presets, the whole UI — is identical
either way. V1 proves that entire chain end to end with nothing else to install, and none of it is
throwaway when the bridge arrives.

### The Quest path, for later

**Settled — see `09-quest-and-players.md`.** Both DeoVR and HereSphere expose remote control only
as raw TCP on port 23554, with identical framing, so a bridge is required and **one adapter covers
both**. It does not need a PC in the room: one small always-on process on the home server is
enough, and it's the same box the media library lives on.

Also ruled out there: hosting the app inside the Quest Browser (no Web Bluetooth), and DLNA
(impossible from any browser).

---

## Retiring the desktop app

The intent is that this replaces the Tauri app rather than living alongside it. That makes the
TypeScript engine the canonical one and the Rust engine a reference snapshot — golden-trace
fixtures are a one-time correctness check, not an ongoing sync contract.

**One capability cannot follow.** A browser cannot listen on a port. The desktop app's T-Code
WebSocket server bound to `0.0.0.0` — the thing LAN clients such as Lovense games connect *into* —
has no browser equivalent; a PWA can only dial out.

So full retirement requires the bridge to take that role: listen for T-Code on the LAN, forward to
the phone over `wss://`. Same binary that handles DeoVR and HereSphere. The bridge is optional for
V1 and mandatory for the end state.

## Script library

**The video is the harder half of this problem.** VR video is multi-GB, so it will be streamed from
a home server rather than copied to the phone. Once that server exists, scripts should be fetched
from the same place — which makes name-sync automatic, because there is only one copy and the app
asks for `<playing-file-name>.funscript` alongside it.

One requirement the video doesn't have: **funscripts need CORS.** A `<video src>` loads
cross-origin freely; `fetch()` for JSON does not. A single header on the media path covers it.

Order of implementation:

1. **`OpfsRepository`** — files picked with `<input type="file" multiple>`, cached in OPFS. Zero
   infrastructure, unblocks development and testing immediately. iOS has no directory picker, but
   the Files app can mount an SMB share, so importing from a NAS is tolerable.
2. **`HttpIndexRepository`** — a directory listing or `index.json` served with CORS from the same
   box as the media. This is the real answer.

Both behind one `ScriptRepository` interface, defined from day one so the second is additive.

**Rejected: peer-to-peer drag and drop.** A WebRTC data channel between a desktop page and the
phone needs a signalling server — infrastructure built to avoid building infrastructure — and only
moves files, which HTTP does better and permanently. Viable later as a plugin, not as core.

### Multi-script convention — already a standard

The instinct to use `content.<something>.funscript` is exactly the existing community convention.
From MFP's `DeviceSettings.cs`:

| Suffix | Axis | Meaning |
|---|---|---|
| *(none)* / `stroke` / `up` | **L0** | Up/Down — the default |
| `surge` / `forward` | L1 | Forward/Backward |
| `sway` / `left` | L2 | Left/Right |
| `twist` / `yaw` | R0 | Twist |
| `roll` | R1 | Roll |
| `pitch` | R2 | Pitch |

So `movie.funscript` → L0, `movie.roll.funscript` → R1, and so on. Match on the filename stem,
offer every discovered suffix as a linkable source, default to the bare `.funscript` on L0.

This means **no new mapping design is needed.** A funscript position becomes an axis value; the
existing `ParameterSource` model consumes axis values already. Presets carry over.

---

## UI

Mobile-first, and the target is **everything visible without scrolling**.

### Top bar

Left to right: **bolt icon** (the wordmark shrinks to just the mark to save width), gamepad
indicator, output-connection indicator with its popover, play/pause, settings gear.

Connection settings become "where playback is coming from" rather than a WebSocket server.

### Main surface

- **Preset row** — presets plus gamepad jump-to-preset. The engine selector leaves the main page
  (see below), so the preset row gets that space.
- **Channel A and B, both visible at once.** Stack the cards, but re-weight them: *intensity* and
  *frequency* get large controls, *frequency balance* and *intensity balance* get small ones —
  they're rarely touched. That's what buys enough vertical room for both channels on one screen.
- **Position indicator** — the little line showing current value. Keep it.
- **Funscript graph** in place of the input monitor. Seeing the script scroll past is more useful
  than the output waveform.

### Linking panel — decided 2026-07-28

Explored three layouts (row-and-sheet, source cards, patch grid) and settled on **the smallest
delta from the current desktop app**:

- **Channel cards stay the main screen.** Tapping a parameter row opens a **sheet**.
- The sheet is where linking is configured, and deliberately resembles today's link popover so it
  translates rather than needing to be relearned.
- **Source** is a list: funscripts discovered for the currently playing content, plus gamepad axes
  and analog triggers when a controller is connected, plus *none / static*. Static is an option in
  that list, exactly as the current chip toggle works.
- **Curve**: linear or inverse. **Range**: min/max. **Midpoint**: optional, minor.
- With source *none*, the sheet offers a single static value instead of a range.

**Rejected for V1:** a separate source-management screen, and a bottom tab bar. Both add a whole
second experience for no V1 benefit. The source-centric layout remains an interesting later idea —
it matches how configuration is actually reasoned about (one source driving several parameters) —
but it is not what gets built now.

Presets carry linking as well as values, so switching a preset moves parameters between sources.
That is accepted behaviour, not a problem to engineer around.

Reference config in use today: L0 → intensity and frequency, L0-inverse → intensity and frequency.

- **Curves:** linear and inverse are required. Port the rest only if cheap.
- **Midpoint toggle:** nice to have, droppable for V1.
- **Transformations:** out. Too experimental.

### Engine

**`V2Sustained` only.** Move the engine selector into settings, or hide it entirely.

> Note: `CLAUDE.md` lists the engines as v1 / v2-Smooth / v2-Balanced / v2-Detailed. That's stale —
> `processing.rs` also has `V2Dynamic` and `V2Sustained`. Worth fixing in the existing repo.

### Gamepad

Full port. Bindings, and **the chord system stays** — it's how the app is driven in VR. Keyboard
shortcuts go away; there's no keyboard.

### Settings

In: no-input behavior, safety limits, playback/connection config, gamepad bindings, engine
(hidden). Update rate and save rate only if they port cleanly.

### Cut from V1

Input monitor / output waveform view, transformations, non-`V2Sustained` engines, keyboard
shortcuts, extra curve types.

---

## Repo

**New repo.** Reasons:

- Different product, different platform, different licence intent (open source, plugin ecosystem)
- The desktop app keeps working and shipping while this is built
- Contributors need a repo that isn't also a Tauri desktop app

### The engine is TypeScript, not WASM — decided 2026-07-28

The earlier plan was to extract `coyote-core` as a Rust crate and compile it to WASM for the
browser. **That is no longer the plan.** With V1 scoped to a single engine (`V2Sustained`), two
curves, and no transformations, the portable surface shrinks from ~4,300 lines to roughly
1,500–2,000, and 10 Hz makes the performance argument irrelevant.

Consequences:

- **No `crates/` directory in this repo.** It is purely a PWA; root layout is correct.
- The correctness guarantee WASM would have given comes instead from **golden-trace fixtures**
  generated from the real Rust `V2Sustained` and asserted against by the TypeScript engine. Those
  live in `docs/spikes/pwa/fixtures/` in the desktop repo.
- Fixtures are a **one-time snapshot**, not an ongoing sync contract, because the desktop app is
  being hollowed out into the bridge rather than maintained in parallel.
- The bridge stays in the **existing** repo — it is what the Tauri app becomes, keeping `net.rs`,
  `buttplug/`, `input_bus.rs` and `tcode_input.rs` and shedding the engine, BLE and UI.

---

## Next step

The extraction spike, time-boxed to one day: does `coyote-core` come out of `src-tauri` cleanly,
and does it build for `wasm32` producing byte-identical B0 output against a fixed input trace?

That's the last unknown. Everything above is decided.
