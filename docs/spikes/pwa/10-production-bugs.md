# Production Bugs Found During the Spike

Found 2026-07-28 while building golden-trace fixtures from the Rust signal engine, and confirmed by
an independent adversarial review. **These were defects in the shipping desktop app, not port
risks.**

**All four are fixed** on the engine side, with tests and regenerated fixtures. Each entry below
keeps the original finding and records what was done.

---

## 1. Inverted intensity range produces constant full-scale output — SAFETY

**Severity: high.** This app drives e-stim hardware.

`src-tauri/src/device.rs` — `scale_intensity` returned `min` verbatim whenever `max <= min`.

Observed in `docs/spikes/pwa/fixtures/range-inverted.json`, channel A configured
`rangeMin: 200, rangeMax: 0`:

```
t+0    scaled_intensity=200   raw=0
t+900  scaled_intensity=200   raw=194
```

**The channel emitted 200 — device maximum — from t+0, with the axis at rest and the engine
producing 0.** Output was constant and completely independent of input.

`range-degenerate-equal.json` shows the benign sibling: `min == max` pins at the shared value,
which is a legitimate "hold this channel at a constant".

**Trigger:** a user enters the range endpoints in the wrong order.

### Fixed — endpoints are ordered, not inverted

`device::scale_intensity` sorts `min`/`max` before mapping, and
`ParameterLinkConfig::ordered_range()` does the same on the resolver side (used by
`resolve_link_at_time`'s range mapping and by `resolver::build_channel_snapshot`'s
`range_min`/`range_max`). `200..0` now behaves exactly like `0..200`.

**Why ordering rather than honouring the inversion:**

- The app has no concept of an inverted range. `App.svelte`'s own bound adjusters
  (`adjustIntensityRangeBound`, `adjustFrequencyRangeBound`) clamp each endpoint against the
  other, so a transposed range cannot be produced through the UI at all. It can only arrive from
  a hand-edited `presets.json` or a stale file — i.e. it is a mistake, not an expression of
  intent.
- `curve: inverse` already *is* the supported way to make output fall as input rises, and it
  composes with a normal range. Reading a transposed range as a second, redundant inversion
  mechanism would make two different configs mean the same thing.
- Honouring the inversion would not have removed the hazard. `lerp(200, 0, 0) = 200`: an
  inverted range puts **full output at a resting axis** either way. Ordering is the only reading
  under which a mis-entered range fails safe.

**Why not reject at the configuration boundary:** there is no error path from the tick loop back
to the UI, and refusing the config would leave the channel running on whatever it had before —
less predictable than mapping over the span the user actually typed. Ordering also can't be
bypassed: it happens at every read, so a config that slips past any future validation still lands
on the same behaviour in both code paths.

**Correction to the original finding:** the write-up said the resolver's `lerp` honoured the
inversion and that `resolved.intensity` therefore disagreed with `scaled_intensity`. It does not.
`build_channel_snapshot` synthesizes `resolved.intensity` for engine-path links from
`v2.get_value_at(now)` and never calls `lerp`. The genuine disagreement was between
`scale_intensity` (intensity, engine path) and `lerp` (frequency, the balances, and `bp:`-routed
intensity). Both now order their endpoints, so the two agree by construction — pinned by
`device::tests::scale_intensity_agrees_with_the_resolver_on_a_transposed_range`.

**Tests:** `device::tests::scale_intensity_orders_a_transposed_range`,
`scale_intensity_keeps_a_zero_width_range_constant`,
`scale_intensity_agrees_with_the_resolver_on_a_transposed_range`,
`modulation::tests::ordered_range_sorts_a_transposed_range`,
`resolve_orders_a_transposed_range_instead_of_inverting`.

---

## 2. Ramped T-Code commands overshoot to full scale before the ramp completes — SAFETY

**Severity: high in principle, scales with the commanded interval.** Unlike bug 1 this needed **no
misconfiguration** — an ordinary `I<ms>` T-Code command was enough.

`processing.rs::apply_tcode` stores the commanded **target** in the downsampler, while the V2 ramp
stores interpolated positions. `Channel::next_raw_values` took `downsample_dynamic` whenever
`has_samples_in_window(window_start, now)` and only otherwise consulted the ramp. So the tick whose
window contained the command emitted the *target*, while the ramp had barely started.

Observed in `ramped-targets.json` — channel A's device-facing intensity byte for a command asking
to ease to full over 600 ms:

```
t+200  b0[2]=  0    <- "L0 -> 1.0 over 600ms" arrives
t+300  b0[2]=200    <- full scale, 500 ms early
t+400  b0[2]=200
t+500  b0[2]=200    <- held (peak-hold sustains it)
t+600  b0[2]=125    <- drops
```

For the first three ticks the ramp was not merely imprecise, it was **inverted** — and it erred
toward more output, sooner.

**How much this mattered depended on the interval.** MultiFunPlayer emits the suffix
(`DeviceAxis.cs:47` → `L0500I33`) with its interval typically equal to the output update period —
tens of milliseconds — so the overshoot window was roughly one tick. The failure was significant
for any source sending genuinely long eases, which is what a funscript-driven source or a smoothing
layer produces.

### Fixed — the ramp outranks the downsampler while it is in flight

`V2ChannelState::is_ramping_at(timestamp)` returns true only when
`ramp_end_time > ramp_start_time && timestamp < ramp_end_time`. `Channel::next_raw_values`
evaluates it at `window_start` and skips the downsampler branch when it holds, on all four V2 arms
(`V2Smooth`, `V2Balanced`, `V2Detailed`, `V2Dynamic`/`V2Sustained`). `V3Predictive` is untouched.

**Why this is safe for un-ramped input.** `set_target(.., 0, ts)` — every command without an
`I<ms>` suffix — leaves `ramp_end_time == ramp_start_time`, as do `Default` and `reset()`. So
`is_ramping_at` is false for the entire lifetime of a channel that never receives a ramped command,
and the dispatch is bit-for-bit what it was. `processing::tests::unramped_commands_keep_the_downsampler_dispatch`
proves it by comparing the dispatch against the downsampler directly, on all five V2 engines. Only
the two fixtures carrying `interval_ms` moved; the other 24 are byte-identical across this change.

**Behaviour change worth knowing about:** the same dispatch also dropped ramp-*downs* early. A
"ease to 0 over 500ms" used to reach 0 on the next tick; it now takes the full 500ms it asked for.
In `ramped-targets` that shows as tick +1500 reading `[200,190,180,170]` where it used to read
`[0,0,0,0]`, and — through the 200ms peak-hold — one tick of higher output at +1700 (200 vs 160).
This is correct behaviour, but it means an `I<ms>` command is no longer an accidental fast stop.
Pause still sends the all-stop frame immediately (`device::send_zero_command`).

**Tests:** `processing::tests::ramped_command_does_not_overshoot_to_its_target`,
`unramped_commands_keep_the_downsampler_dispatch`,
`is_ramping_at_is_false_for_instant_and_fresh_state`.

---

## 3. `NoInputBehavior::Decay` was a no-op for any `decay_ms <= 1000`

`modulation.rs` gated staleness on `age_ms > 1000` while decay progress was
`(age_ms / decay_ms).min(1.0)`. The Decay branch was therefore only ever *entered* once
`age_ms > 1000`; with `decay_ms <= 1000` progress was already `>= 1.0` at that moment and the
expression collapsed to `state.value * 0.0`.

**Decay was mathematically incapable of returning anything but zero unless `decay_ms > 1000`.** It
was an alias for `Zero`, and the app ships `no_input_decay_ms = 1000`.

### Fixed — progress is measured from the staleness threshold

The `1000` is now the named `modulation::STALENESS_THRESHOLD_MS`, and Decay computes
`((age_ms - STALENESS_THRESHOLD_MS) / decay_ms).min(1.0)`.

**Why this rather than enforcing `decay_ms > staleness threshold` in configuration:** measuring
from the threshold is the only reading under which `Hold → Decay` is continuous. At the instant
staleness begins this returns `state.value` — exactly what `Hold` returns — and falls from there.
Timing from the last sample makes decay engage with a discontinuous jump to
`value * (1 - 1000/decay_ms)`, which is a step change in device output at a moment the user has
already stopped providing input. A config rule would also leave the shipped default silently
clamped upward and require every existing preset to be migrated; this makes the setting mean what
its label says at any value.

`decay_ms == 0` needs no special case: the branch is only reachable when `stale_for_ms >= 1`, so
the division yields `+inf` and `.min(1.0)` folds it to a completed decay.

**Tests:** `modulation::tests::decay_ramps_at_the_shipped_default_decay_ms`,
`decay_is_distinguishable_from_zero`, `decay_with_zero_window_is_zero_not_nan`.

---

## 4. `processing::tests::test_parse_tcode_with_interval` asserted the wrong value

Pre-existing failure, unrelated to any spike work — identical at `HEAD`.

```rust
let commands = parse_tcode("R2750I1000");
assert!((commands[0].value - 0.25).abs() < 0.01);   // failed; actual is 0.75
```

`parse_tcode` normalises by digit count, so `R2750` is `0.75`. The parser is correct per the T-Code
spec and the test's expectation was simply wrong.

### Fixed — expectation corrected to `0.75`.

---

## Fixture impact

Regenerated with `cargo test --manifest-path src-tauri/Cargo.toml golden`. Nine files moved:

| Fixture | Change |
|---|---|
| `range-inverted` | Both channels now track their input across the ordered span instead of pinning at `range_min` (A `200` flat → `0..194`; B `150` flat → `50..150`). |
| `range-degenerate-equal` | Description only. Every tick byte-identical — ordering an already-equal pair is a no-op. |
| `ramped-targets` | The tick containing each ramped command (+300, +1500) now interpolates instead of jumping to the endpoint. Knock-on through the peak-hold at +1700. All other ticks unchanged. |
| `ramp-retarget-midflight` | Same, at +300, +700 and +1300. The +600 re-anchor literal `[100,100,100,100]` is preserved. |
| `no-input-decay` | Decay now leaves 180.10 Hz at the staleness threshold (+1400) and reaches the 1 Hz floor at +4300, instead of engaging already 43% depleted and snapping to the floor at +3300. 29 of 46 ticks now differ from `no-input-zero`; they used to be byte-identical. |
| `no-input-{hold,zero,default}` | Tick count 36 → 46 only, so the trace outlasts the longer decay window. Every pre-existing tick byte-identical. |
| `index.json` | Descriptions and tick counts. |

The other 18 fixtures are unchanged. Determinism re-verified by generating twice in separate
processes at different wall-clock times and diffing the folder.

---

## Not a bug, but decide before porting

**`peak_fill` is inert under `V2Sustained`.** `processing.rs` hardcodes
`PeakFillStrategy::Forward` on that arm; the setting is only forwarded on the `V2Detailed` arm.
Since V1 is `V2Sustained`-only, the setting cannot affect output. Either hide it or wire it up — do
not port a control that does nothing. **Not addressed here** — out of scope for this change.
