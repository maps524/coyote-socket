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
| `ramped-targets` | T-Code `I<ms>` ramp durations — the only fixtures where V2's interpolation runs |
| `ramp-retarget-midflight` | A new ramped target arriving while the previous ramp is in flight |
| `oscillation-threshold-boundary` | Per-window spread of exactly 39, 40 and 41 device units |
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
| `no-input-{hold,zero,decay,default}` | Input stops, trace crosses the staleness threshold and the full decay window |
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
  // `interval_ms` is the T-Code `I<ms>` ramp duration; the key is omitted
  // when the command carried none. Only `ramped-targets` and
  // `ramp-retarget-midflight` contain it — see "V2 ramp" below.
  "inputs": [
    { "t": 1000000, "axis": "L0", "value": 0.0 },
    { "t": 1000200, "axis": "L0", "value": 1.0, "interval_ms": 600 }
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

### Three fields that will mislead you

- **`peak_fill` is inert.** It is serialized because it is part of engine configuration, but
  `V2Sustained` reaches `downsample_dynamic`, which hardcodes `PeakFillStrategy::Forward` for
  its sub-threshold fallback. The field cannot affect any byte in these fixtures. It only
  matters for `V2Detailed`, which is out of V1 scope.
- **`staticValue` means two different things.** On a `"type": "static"` parameter it is the
  device value verbatim. On a `"type": "linked"` parameter it is only read by
  `NoInputBehavior::Default`, and there it is consumed as a **raw, pre-curve, normalized 0..1
  input** that then runs through the curve and range mapping. In `no-input-default.json` the
  frequency link's `staticValue: 0.75` resolves to `lerp(1, 200, 0.75) = 150.25 Hz`, not
  0.75 Hz.
- **`no_input_decay_ms` is 3000 in the no-input fixtures, not the app default of 1000.** See
  the note under `no-input-decay` in Known gaps — at 1000 the setting is inoperative.

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
5. Per tick, in this order (the order `device.rs::send_device_update` uses):
   1. advance the engines and produce waveform data,
   2. resolve the four per-25ms-slot frequencies over `[now-100, now)`,
   3. resolve the channel parameter snapshot,
   4. range-scale the master intensity, then apply `max_intensity`,
   5. assemble the B0 frame.
6. Assert your frame equals `ticks[i].b0`.

Start by making one fixture pass end to end rather than all of them at once.

On step 5's ordering: in production the frequency pass advances link state that the snapshot
pass then consumes rather than re-resolving, which matters when a stateful transform is
attached to frequency. **Transforms are out of V1 scope, so within these fixtures that
coupling has no observable effect** — `frequency_hz == freq_slots_hz[3]` in all 868
channel-ticks, and a port that skips the stash and simply copies slot 3 is indistinguishable
here. Preserve the ordering anyway if you intend to add transforms later, but know that no
fixture will tell you if you got it wrong. Review it by hand.

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

## The V2 ramp

Worth calling out separately, because it is the one part of the engine that is easy to fake
and dangerous to get wrong.

A T-Code command may carry an `I<ms>` suffix (`L0500I600`) giving a ramp duration. That
duration reaches `V2ChannelState::set_target(value, duration_ms, ts)`. When it is zero —
which it is for every command without the suffix — `ramp_end_time == ramp_start_time` and
`get_value_at` returns the target on its first branch. **The interpolation body never runs.**

So a `getValueAt()` implemented as `return this.targetValue` passes every fixture that has no
`interval_ms`, and silently converts every ramped command into an instant jump on hardware.
That fails in the unsafe direction. Two fixtures exist specifically to catch it:

- `ramped-targets` — sparse ramped commands with long gaps. Watch the two-stage response: the
  tick immediately after a command sees the sample in its downsampler window and jumps to the
  target, then later ticks fall back to the ramp evaluated over `[now-100, now-25]` and read
  *lower*. That non-monotonic shape is genuine engine-dispatch behaviour in
  `Channel::next_raw_values`, not a fixture artefact.
- `ramp-retarget-midflight` — a new target arriving mid-ramp. This is the only trace that
  exercises `ramp_start_value = self.get_value_at(timestamp)` against a partially completed
  ramp. A port that re-anchors from `current_value` (never updated while a ramp is running) or
  from the previous target produces a discontinuity here and nowhere else.

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
- **`NoInputBehavior::Decay` is inoperative at the app's default setting**, and the fixtures
  deliberately do not reproduce that. The staleness gate is `age_ms > 1000` while decay
  progress is `(age_ms / decay_ms).min(1.0)`, so with `no_input_decay_ms <= 1000` the Decay
  branch is first entered only once progress has already reached 1.0 — it can return nothing
  but zero, making it identical to `Zero`. The app ships `no_input_decay_ms = 1000`. These
  fixtures use 3000 so the ramp is observable and the four no-input traces are actually
  distinct. Port the arithmetic as written; just know the shipped default makes the feature
  a no-op. Worth fixing at source.
- **`delay_ms` is never set**, so the delayed-replay path in
  `replay_pending_intensity_samples` — including the `INTENSITY_REPLAY_FLOOR_MS = 200` bound
  on how far a stale watermark can reach back — runs only in its zero-delay form. Per-parameter
  input delay is out of V1 scope.
- **The peak-hold buffer's prune cutoff is untested.** `IntensityPeakHold::observe` prunes at
  1000ms while every read uses a 200ms window, so the prune is unobservable: a port that
  prunes at the read window instead agrees on every fixture here. Only a read window longer
  than 200ms would distinguish them, and nothing reads longer.
