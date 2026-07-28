# Golden-trace fixtures

Input→output traces captured from the **Rust signal engine in `src-tauri/`**, so a
TypeScript reimplementation can be proven equivalent instead of assumed equivalent.

These are a one-time correctness check against a reference snapshot, not an ongoing sync
contract — see `../08-v1-scope.md`, "Retiring the desktop app".

## Scope

Per the V1 scope doc, the fixtures cover only what V1 ships:

- **Engine:** `V2Sustained` only.
- **Curves:** `linear` and `inverse` only.
- **Transforms:** none. Out of V1 scope, so out of the fixtures.

## What's here

| File | Covers |
|---|---|
| `index.json` | Manifest of every fixture, plus the all-stop `zero_b0` frame |
| `static-intensity-endpoints` | Static intensity at 0 and 200; no input at all |
| `static-intensity-midpoint-and-overflow` | Static 100 and 255 — pins the 200 clamp on the static branch |
| `linked-linear-full-axis-sweep` | Both channels linear over 0..200, axis swept 0→1 |
| `linked-inverse-full-axis-sweep` | Same sweep with the inverse curve |
| `linked-linear-vs-inverse-same-axis` | The scope doc's reference config: L0 → A linear, B inverse |
| `range-partial-window` | Narrow sub-ranges (50..150, 120..140) — `scale_intensity` arithmetic + rounding |
| `range-inverted` | `range_min > range_max` on both channels |
| `range-degenerate-equal` | `range_min == range_max` |
| `intensity-soft-cap` | Per-channel cap (A=80) flat-topping the intensity byte |
| `sustained-single-transient` | One 20ms spike, then silence — peak-hold in isolation |
| `sustained-fast-flicking` | 0/1 alternating every 10ms for 600ms, then dead stop |
| `sustained-step-changes` | Step ladder with each level held long enough to observe hold + decay |
| `sustained-peak-hold-boundary` | Sub-tick pulses placed on the 200ms hold-window edge |
| `frequency-period-boundaries` | Every branch boundary of `convert_period` |
| `frequency-per-slot-sweep` | 25ms-cadence frequency input → four different period bytes in one frame |
| `balance-linked` | Frequency/intensity balance linked across 0..255 |
| `midpoint-distance-from-center` | `midpoint` on vs off, same input |
| `channels-independent` | Fully asymmetric channels driven by unrelated input |
| `no-input-{hold,zero,decay,default}` | Input stops, trace crosses the 1000ms staleness threshold |
| `no-input-at-all` | Everything linked, zero input events — cold start |

## Format

Each fixture is one self-describing JSON object.

```jsonc
{
  "schema_version": 1,
  "name": "linked-linear-full-axis-sweep",
  "description": "…what this exercises…",

  // Engine configuration for the whole trace.
  "engine": "v2-sustained",
  "peak_fill": "forward",
  "no_input_behavior": "hold",
  "no_input_decay_ms": 1000,

  // Timeline. Ticks run at start_time_ms + i * tick_interval_ms.
  "start_time_ms": 1000000,
  "tick_interval_ms": 100,
  "tick_count": 14,

  "channel_a": {
    // Exactly the app's own preset shape (camelCase) — paste-compatible
    // with presets.json.
    "config": {
      "frequency":        { "type": "static", "staticValue": 100, … },
      "frequencyBalance": { … },
      "intensityBalance": { … },
      "intensity": {
        "type": "linked", "sourceAxis": "L0",
        "rangeMin": 0, "rangeMax": 200, "curve": "linear",
        "curveStrength": 2, "midpoint": true      // optional keys omitted when unset
      }
    },
    // settings.general.channel_a_max_intensity — the "soft mode" cap.
    "max_intensity": 200
  },
  "channel_b": { … },

  // Axis writes, chronological. `t` is absolute, same clock as the ticks.
  "inputs": [
    { "t": 1000000, "axis": "L0", "value": 0.0 },
    { "t": 1000020, "axis": "L0", "value": 0.0166…, "interval_ms": 500 }
  ],

  "ticks": [
    {
      "t": 1000000,
      "channel_a": {
        "raw_intensity": 130,          // engine master, post peak-hold, pre-range
        "scaled_intensity": 130,       // post-range, post-cap — the B0 intensity byte
        "waveform_intensity": [93, 95, 98, 100],  // B0 waveform bytes (relative 0-100)
        "raw_values": [120, 123, 127, 130],       // per-slot engine output, device units
        "frequency_hz": 100.0,
        "freq_slots_hz": [100.0, 100.0, 100.0, 100.0],
        "period_slots": [10, 10, 10, 10],         // B0 frequency bytes
        "freq_balance": 128,           // BF command
        "int_balance": 128,            // BF command
        "range_min": 0, "range_max": 200,
        "intensity_is_static": false,
        // Resolver's own view (post-curve, pre-engine), snake_case keys:
        // frequency / frequency_balance / intensity_balance / intensity.
        "resolved": { "intensity": {
          "raw_input": 0.666…,            // bus value at target_time_ms
          "normalized_pre_range": 0.665,  // post-curve, clamped 0..1
          "device_value": 133,            // post-range, resolver's view
          "target_time_ms": 1000800,
          "source_axis": "L0"             // key omitted entirely when static
        }, … }
      },
      "channel_b": { … },
      "b0": [176, 15, 130, 130, 10, 10, 10, 10, 93, 95, 98, 100, …]
    }
  ]
}
```

### `b0` is the contract

**Bytes matter more than floats.** A port is equivalent iff `b0` matches for every tick of
every fixture. The 20-byte layout (`protocol.rs::generate_b0_command`):

| Offset | Bytes | Meaning |
|---|---|---|
| 0 | 1 | `0xB0` header |
| 1 | 1 | serial (always 0) + interpretation A/B (always `0b11` each → `0x0F`) |
| 2 | 1 | channel A intensity, 0-200 |
| 3 | 1 | channel B intensity, 0-200 |
| 4-7 | 4 | channel A per-slot frequency (device period units) |
| 8-11 | 4 | channel A per-slot waveform intensity, 0-100 |
| 12-15 | 4 | channel B per-slot frequency |
| 16-19 | 4 | channel B per-slot waveform intensity |

The per-tick scalar fields exist to make a failure diagnosable — they tell you *where* a port
diverged. They are not the pass condition. `raw_values` in particular is diagnostic only; it
never reaches the device.

`resolved` is the **resolver's** view of the input at this instant (post-curve, post-range,
pre-engine). For intensity it will generally *disagree* with `scaled_intensity`, because the
engine path lags it through downsampling, the V2 ramp and the peak hold. That gap is expected
— it is not a sign either number is wrong.

`index.json` also carries `zero_b0`, the all-stop frame sent on pause
(`device.rs::send_zero_command`).

## How a TypeScript implementation should consume these

1. Read `index.json`; refuse to run if `schema_version` is not the version you support.
2. For each fixture, construct engine state from `engine`, `peak_fill`, `no_input_behavior`,
   `no_input_decay_ms`, and the two `channel_*.config` objects.
3. Run a loop of `tick_count` iterations. At iteration `i`, the tick instant is
   `start_time_ms + i * tick_interval_ms`. **Time is an input, never a clock read** — this is
   the single most important property to preserve in the port.
4. Before each tick, deliver every not-yet-delivered `inputs` entry whose `t <= now`, as an
   axis write with that exact timestamp. An event landing exactly on a tick instant is
   delivered *before* that tick runs, but is not yet inside the tick's `[now-100, now)`
   sampling window — it first affects the following tick.
5. Per tick, in this order (the order `device.rs::send_device_update` uses, and it matters
   because the frequency pass mutates link state the snapshot pass then reads):
   1. advance the engines and produce waveform data,
   2. resolve the four per-25ms-slot frequencies over `[now-100, now)`,
   3. resolve the channel parameter snapshot,
   4. range-scale the master intensity, then apply `max_intensity`,
   5. assemble the B0 frame.
6. Assert your frame equals `ticks[i].b0`.

Start by making one fixture pass end to end rather than all of them at once; the ordering in
step 5 is the usual first thing to get wrong.

## Regenerating

```bash
cargo test --manifest-path src-tauri/Cargo.toml golden
```

That runs four tests:

- `generate_golden_fixtures` — rewrites every file in this folder,
- `golden_fixtures_are_deterministic` — two in-process runs must be byte-identical,
- `golden_b0_frames_are_well_formed` — structural guard on every frame,
- `sustained_fixtures_exercise_peak_hold` — proves the `sustained-*` traces actually reach
  a tick where the peak-hold sustains above the live slot values.

Generation is deterministic, so regenerating against an unchanged engine leaves an **empty
git diff**. A non-empty diff means the engine's behaviour changed — which is exactly the
signal you want.

Determinism has been verified by generating twice in separate processes at different
wall-clock times and diffing the whole folder.

The generator lives in `src-tauri/src/golden.rs`. It is a `#[cfg(test)]` module rather than a
binary because `coyote-socket` is a binary-only Cargo package — with no `lib` target, neither
`src/bin/*.rs` nor `tests/*.rs` can reach `crate::processing`, so an in-crate test module is
the only place that can drive the real code path.

## Known gaps

- **`convert_period`'s `>1000 → 240` branch is unreachable** from the device path and so is
  not covered. `build_channel_snapshot` and `resolve_slot_frequencies` both clamp frequency
  to `>= 1 Hz`, so `frequency_to_period` never returns more than 1000. A port should still
  implement the branch, but no fixture will catch getting it wrong.
- **Inverted and degenerate ranges do not invert the output.** `device.rs::scale_intensity`
  returns `min` verbatim whenever `max <= min`, so the intensity byte pins at `range_min`.
  The resolver's own `lerp` *does* honour the inversion, which is visible in the
  `resolved.intensity` fields — the two disagree. That divergence is real production
  behaviour on the engine path and `range-inverted.json` captures it deliberately. Do not
  "fix" it in a port without deciding it is a bug first.
- **Buttplug (`bp:`) intensity routing is not covered.** Those links bypass the engines and
  run the resolver wholesale; out of V1 scope.
