//! Golden-trace fixture generator (test-only).
//!
//! Walks fixed, deterministic input traces through the **production** signal
//! path and writes the resulting input→output pairs to
//! `docs/spikes/pwa/fixtures/` as JSON. A TypeScript reimplementation of the
//! engine can then be proven equivalent against these bytes rather than
//! eyeballed.
//!
//! # Why a test and not a binary
//!
//! `coyote-socket` is a binary-only Cargo package — there is no `lib` target,
//! so neither `src/bin/*.rs` nor `tests/*.rs` can reach `crate::processing`.
//! An in-crate `#[cfg(test)]` module is the only place that can drive the real
//! code path. This is the standard "golden file generator" shape: running the
//! test suite regenerates the fixtures, and because generation is
//! deterministic, an unchanged engine produces an empty diff.
//!
//! # Determinism
//!
//! The engine's only non-deterministic input is the wall clock. Three
//! functions read it in production (`ProcessingState::get_next_waveform_data`,
//! `resolver::get_per_slot_frequencies`, `resolver::get_resolved_channel_params`);
//! each has a `_at(now_ms)` / pure-per-channel counterpart that takes the
//! instant as an argument, and this generator drives those. `parse_tcode` also
//! stamps `SystemTime::now()`, so the generator constructs `TCodeCommand`
//! values directly with trace-supplied timestamps instead of parsing strings.
//!
//! Everything else — the downsamplers, the V2 ramp, `IntensityPeakHold`, the
//! resolver, `convert_period`, B0 assembly — is a pure function of state plus
//! the injected instant. No threads, no randomness, no `HashMap` iteration on
//! the output path (the bus is only read by key).
//!
//! # Scope
//!
//! Per `docs/spikes/pwa/08-v1-scope.md`: `V2Sustained` only, `linear` and
//! `inverse` curves only, no transforms.

use std::path::PathBuf;

use serde::Serialize;

use crate::device::{build_b0_frame, build_zero_b0_frame, scale_intensity};
use crate::modulation::{
    ChannelConfig, ChannelLinkRuntime, CurveType, NoInputBehavior, ParameterLinkConfig,
};
use crate::processing::{
    ChannelId, PeakFillStrategy, ProcessingEngineType, ProcessingState, TCodeCommand,
};
use crate::protocol::{convert_period, frequency_to_period};
use crate::resolver::{build_channel_snapshot, resolve_slot_frequencies, ChannelResolvedSnapshot};

/// Bumped whenever the fixture JSON shape changes in a way a consumer must
/// notice. Consumers should refuse to run against an unexpected version.
const SCHEMA_VERSION: u32 = 1;

/// Fixed trace epoch. Arbitrary but non-zero and comfortably larger than every
/// lookback window in the engine (peak hold 200ms, axis history 1500ms, replay
/// floor 200ms), so no `saturating_sub` clamps at t=0 distort the first ticks.
const TRACE_EPOCH_MS: u64 = 1_000_000;

/// Device tick period. The production loop is 10Hz.
const TICK_MS: u64 = 100;

// ============================================================================
// Trace specification (input side)
// ============================================================================

#[derive(Debug, Clone, Serialize)]
struct InputEvent {
    /// Absolute timestamp (ms) at which this axis write arrives.
    t: u64,
    axis: String,
    /// Normalized axis value, 0.0..=1.0.
    value: f64,
    /// T-Code ramp duration, if the source supplied one.
    #[serde(skip_serializing_if = "Option::is_none")]
    interval_ms: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
struct ChannelSpec {
    /// Serialized in the app's own preset shape (camelCase), so a fixture's
    /// channel config can be pasted straight into `presets.json`.
    config: ChannelConfig,
    /// Per-channel device intensity cap ("soft mode"), mirroring
    /// `settings.general.channel_{a,b}_max_intensity`. Applied after range
    /// scaling, exactly as `device.rs::send_device_update` does.
    max_intensity: u8,
}

#[derive(Debug, Clone, Serialize)]
struct TraceSpec {
    name: String,
    description: String,
    engine: ProcessingEngineType,
    peak_fill: PeakFillStrategy,
    no_input_behavior: NoInputBehavior,
    no_input_decay_ms: u32,
    start_time_ms: u64,
    /// Nominal tick period. Authoritative only when `tick_offsets_ms` is
    /// absent; either way every `ticks[i].t` carries the absolute instant, so
    /// a consumer should read that rather than recomputing the grid.
    tick_interval_ms: u64,
    tick_count: usize,
    /// Explicit, irregular tick timeline as offsets from `start_time_ms`.
    /// Present only on traces that need a stalled device loop — a real
    /// 10Hz loop can miss deadlines when the app is backgrounded, and some
    /// engine behaviour is only reachable through a gap. Omitted (and the
    /// uniform grid used) for every other fixture.
    #[serde(skip_serializing_if = "Option::is_none")]
    tick_offsets_ms: Option<Vec<u64>>,
    channel_a: ChannelSpec,
    channel_b: ChannelSpec,
    inputs: Vec<InputEvent>,
}

// ============================================================================
// Trace results (output side)
// ============================================================================

#[derive(Debug, Clone, Serialize)]
struct ChannelTick {
    /// Engine master intensity before range scaling (0-200). For
    /// `V2Sustained` this is already the 200ms rolling peak-hold value.
    raw_intensity: u8,
    /// What the B0 intensity byte carries: range-scaled then soft-capped.
    scaled_intensity: u8,
    /// Per-slot relative intensity (0-100) — B0 waveform bytes.
    waveform_intensity: [u8; 4],
    /// Per-slot engine output in device units (0-200), before normalization
    /// to relative. Diagnostic only; not part of the B0 frame.
    raw_values: [u8; 4],
    /// Scalar frequency from the resolver snapshot (Hz, clamped 1..=200).
    frequency_hz: f64,
    /// Per-25ms-slot frequency (Hz) driving the B0 period bytes.
    freq_slots_hz: [f64; 4],
    /// `convert_period(frequency_to_period(hz))` per slot — the actual B0
    /// frequency bytes, restated here for readability.
    period_slots: [u8; 4],
    freq_balance: u8,
    int_balance: u8,
    range_min: u8,
    range_max: u8,
    intensity_is_static: bool,
    /// Full resolver view of all four parameters at this instant.
    resolved: ChannelResolvedSnapshot,
}

#[derive(Debug, Clone, Serialize)]
struct TickRecord {
    /// Absolute tick instant (ms).
    t: u64,
    channel_a: ChannelTick,
    channel_b: ChannelTick,
    /// The complete 20-byte B0 frame. **This is the contract** — a port is
    /// equivalent iff these bytes match.
    b0: Vec<u8>,
}

#[derive(Debug, Clone, Serialize)]
struct Fixture {
    schema_version: u32,
    #[serde(flatten)]
    spec: TraceSpec,
    ticks: Vec<TickRecord>,
}

// ============================================================================
// Runner
// ============================================================================

/// Replay one trace through the production path.
///
/// Per tick, in the same order `device.rs::send_device_update` runs:
/// 1. deliver every input event whose timestamp has arrived,
/// 2. `ProcessingState::get_next_waveform_data_at` (advances engines),
/// 3. `resolver::resolve_slot_frequencies` (advances frequency link state,
///    stashes the latest sub-slot sample),
/// 4. `resolver::build_channel_snapshot` (consumes the stash),
/// 5. range scale + soft cap,
/// 6. `device::build_b0_frame`.
fn run_trace(spec: &TraceSpec) -> Vec<TickRecord> {
    let mut state = ProcessingState::default();
    state.options.processing_engine = spec.engine;
    state.options.peak_fill = spec.peak_fill;
    state.no_input_behavior = spec.no_input_behavior.clone();
    state.no_input_decay_ms = spec.no_input_decay_ms;

    for (id, cs) in [
        (ChannelId::A, &spec.channel_a),
        (ChannelId::B, &spec.channel_b),
    ] {
        let ch = state.channel_mut(id);
        ch.link_runtime = ChannelLinkRuntime::for_config(&cs.config);
        ch.config = cs.config.clone();
    }

    // Inputs are authored in chronological order; sort defensively so a
    // hand-edited trace can't silently change delivery order.
    let mut inputs = spec.inputs.clone();
    inputs.sort_by_key(|e| e.t);
    let mut next_input = 0usize;

    // Explicit timeline when the trace needs a stalled loop, uniform grid
    // otherwise.
    let tick_times: Vec<u64> = match &spec.tick_offsets_ms {
        Some(offsets) => offsets.iter().map(|o| spec.start_time_ms + o).collect(),
        None => (0..spec.tick_count)
            .map(|i| spec.start_time_ms + i as u64 * spec.tick_interval_ms)
            .collect(),
    };

    let mut ticks = Vec::with_capacity(tick_times.len());

    for now in tick_times {
        while next_input < inputs.len() && inputs[next_input].t <= now {
            let e = &inputs[next_input];
            state.process_command(&TCodeCommand {
                axis: e.axis.clone(),
                value: e.value,
                interval_ms: e.interval_ms,
                received_at: e.t,
            });
            next_input += 1;
        }

        let (wf_a, wf_b) = state.get_next_waveform_data_at(now);

        let window_start = now.saturating_sub(100);
        let behavior = state.no_input_behavior.clone();
        let decay_ms = state.no_input_decay_ms;
        let engine = state.options.processing_engine;

        let (freq_slots_a, freq_slots_b) = {
            let (bus, channels) = state.split_bus_and_channels();
            let [a, b] = channels;
            (
                resolve_slot_frequencies(a, bus, &behavior, decay_ms, now, window_start),
                resolve_slot_frequencies(b, bus, &behavior, decay_ms, now, window_start),
            )
        };

        let ((params_a, snap_a), (params_b, snap_b)) = {
            let (bus, channels) = state.split_bus_and_channels();
            let [a, b] = channels;
            (
                build_channel_snapshot(a, bus, &behavior, decay_ms, engine, now),
                build_channel_snapshot(b, bus, &behavior, decay_ms, engine, now),
            )
        };

        let scaled_a = if params_a.intensity_is_static {
            wf_a.intensity
        } else {
            scale_intensity(wf_a.intensity, params_a.range_min, params_a.range_max)
        }
        .min(spec.channel_a.max_intensity.min(200));
        let scaled_b = if params_b.intensity_is_static {
            wf_b.intensity
        } else {
            scale_intensity(wf_b.intensity, params_b.range_min, params_b.range_max)
        }
        .min(spec.channel_b.max_intensity.min(200));

        let b0 = build_b0_frame(
            scaled_a,
            scaled_b,
            freq_slots_a,
            freq_slots_b,
            wf_a.waveform_intensity,
            wf_b.waveform_intensity,
        );

        let periods = |hz: [f64; 4]| -> [u8; 4] {
            std::array::from_fn(|i| convert_period(frequency_to_period(hz[i])))
        };

        ticks.push(TickRecord {
            t: now,
            channel_a: ChannelTick {
                raw_intensity: wf_a.intensity,
                scaled_intensity: scaled_a,
                waveform_intensity: wf_a.waveform_intensity,
                raw_values: wf_a.raw_values,
                frequency_hz: params_a.frequency,
                freq_slots_hz: freq_slots_a,
                period_slots: periods(freq_slots_a),
                freq_balance: params_a.freq_balance,
                int_balance: params_a.int_balance,
                range_min: params_a.range_min,
                range_max: params_a.range_max,
                intensity_is_static: params_a.intensity_is_static,
                resolved: snap_a,
            },
            channel_b: ChannelTick {
                raw_intensity: wf_b.intensity,
                scaled_intensity: scaled_b,
                waveform_intensity: wf_b.waveform_intensity,
                raw_values: wf_b.raw_values,
                frequency_hz: params_b.frequency,
                freq_slots_hz: freq_slots_b,
                period_slots: periods(freq_slots_b),
                freq_balance: params_b.freq_balance,
                int_balance: params_b.int_balance,
                range_min: params_b.range_min,
                range_max: params_b.range_max,
                intensity_is_static: params_b.intensity_is_static,
                resolved: snap_b,
            },
            b0,
        });
    }

    ticks
}

// ============================================================================
// Spec-building helpers
// ============================================================================

fn stat(v: f64) -> ParameterLinkConfig {
    ParameterLinkConfig::static_source(v)
}

fn link(axis: &str, min: f64, max: f64, curve: CurveType) -> ParameterLinkConfig {
    ParameterLinkConfig::linked_source(axis, min, max, curve)
}

/// Channel config with the three non-intensity parameters left static at the
/// app's own defaults (100Hz, balance 128/128).
fn cfg_default_params(intensity: ParameterLinkConfig) -> ChannelConfig {
    ChannelConfig {
        frequency: stat(100.0),
        frequency_balance: stat(128.0),
        intensity_balance: stat(128.0),
        intensity,
    }
}

fn chan(config: ChannelConfig) -> ChannelSpec {
    ChannelSpec {
        config,
        max_intensity: 200,
    }
}

fn chan_capped(config: ChannelConfig, max_intensity: u8) -> ChannelSpec {
    ChannelSpec {
        config,
        max_intensity,
    }
}

struct SpecBuilder {
    spec: TraceSpec,
}

impl SpecBuilder {
    fn new(name: &str, description: &str, a: ChannelSpec, b: ChannelSpec) -> Self {
        Self {
            spec: TraceSpec {
                name: name.to_string(),
                description: description.to_string(),
                engine: ProcessingEngineType::V2Sustained,
                peak_fill: PeakFillStrategy::default(),
                no_input_behavior: NoInputBehavior::Hold,
                no_input_decay_ms: 1000,
                start_time_ms: TRACE_EPOCH_MS,
                tick_interval_ms: TICK_MS,
                tick_count: 10,
                tick_offsets_ms: None,
                channel_a: a,
                channel_b: b,
                inputs: Vec::new(),
            },
        }
    }

    fn ticks(mut self, n: usize) -> Self {
        self.spec.tick_count = n;
        self
    }

    /// Drive the trace from an explicit, irregular tick timeline instead of
    /// the uniform grid — for simulating a device loop that missed deadlines.
    fn tick_offsets(mut self, offsets: Vec<u64>) -> Self {
        self.spec.tick_count = offsets.len();
        self.spec.tick_offsets_ms = Some(offsets);
        self
    }

    fn no_input(mut self, behavior: NoInputBehavior, decay_ms: u32) -> Self {
        self.spec.no_input_behavior = behavior;
        self.spec.no_input_decay_ms = decay_ms;
        self
    }

    /// One axis write at `offset_ms` after the trace epoch.
    fn at(mut self, offset_ms: u64, axis: &str, value: f64) -> Self {
        self.spec.inputs.push(InputEvent {
            t: TRACE_EPOCH_MS + offset_ms,
            axis: axis.to_string(),
            value,
            interval_ms: None,
        });
        self
    }

    /// One axis write at `offset_ms` carrying a T-Code ramp duration
    /// (`I<ms>`). This is what reaches `V2ChannelState::set_target` as a
    /// non-zero `duration_ms` and makes the ramp interpolation body run —
    /// with `interval_ms` absent the ramp collapses to an instant jump and
    /// `get_value_at` short-circuits on its first branch forever.
    fn at_ramp(mut self, offset_ms: u64, axis: &str, value: f64, interval_ms: u32) -> Self {
        self.spec.inputs.push(InputEvent {
            t: TRACE_EPOCH_MS + offset_ms,
            axis: axis.to_string(),
            value,
            interval_ms: Some(interval_ms),
        });
        self
    }

    /// Alternating `lo`/`hi` writes across `[start_ms, end_ms)` at `step_ms`.
    /// Unlike `flick`, the span is expressed as a half-open range so phases
    /// can be chained on tick boundaries without their samples ever sharing a
    /// downsampler window.
    fn phased_flick(
        mut self,
        start_ms: u64,
        end_ms: u64,
        step_ms: u64,
        axis: &str,
        lo: f64,
        hi: f64,
    ) -> Self {
        let mut i = 0u64;
        let mut t = start_ms;
        while t < end_ms {
            self.spec.inputs.push(InputEvent {
                t: TRACE_EPOCH_MS + t,
                axis: axis.to_string(),
                value: if i % 2 == 0 { lo } else { hi },
                interval_ms: None,
            });
            i += 1;
            t += step_ms;
        }
        self
    }

    /// `count` writes on `axis` at `step_ms` intervals starting at
    /// `offset_ms`, linearly interpolating `from`→`to` across them.
    fn sweep(
        mut self,
        offset_ms: u64,
        step_ms: u64,
        count: usize,
        axis: &str,
        from: f64,
        to: f64,
    ) -> Self {
        for i in 0..count {
            let t = (i as f64) / ((count - 1).max(1) as f64);
            self.spec.inputs.push(InputEvent {
                t: TRACE_EPOCH_MS + offset_ms + i as u64 * step_ms,
                axis: axis.to_string(),
                value: from + (to - from) * t,
                interval_ms: None,
            });
        }
        self
    }

    /// `count` writes on `axis` at `step_ms` intervals alternating `lo`/`hi` —
    /// the "pole flicking" input pattern `V2Sustained` exists to handle.
    fn flick(
        mut self,
        offset_ms: u64,
        step_ms: u64,
        count: usize,
        axis: &str,
        lo: f64,
        hi: f64,
    ) -> Self {
        for i in 0..count {
            self.spec.inputs.push(InputEvent {
                t: TRACE_EPOCH_MS + offset_ms + i as u64 * step_ms,
                axis: axis.to_string(),
                value: if i % 2 == 0 { lo } else { hi },
                interval_ms: None,
            });
        }
        self
    }

    fn build(self) -> TraceSpec {
        self.spec
    }
}

/// Axis value that makes a `range_min=1.0, range_max=200.0` linear link
/// resolve to `hz`. Used by the frequency fixtures to land on
/// `convert_period`'s branch boundaries.
fn axis_for_hz(hz: f64) -> f64 {
    ((hz - 1.0) / 199.0).clamp(0.0, 1.0)
}

// ============================================================================
// The fixture set
// ============================================================================

fn all_specs() -> Vec<TraceSpec> {
    let mut out = Vec::new();

    // --- Static intensity -------------------------------------------------
    out.push(
        SpecBuilder::new(
            "static-intensity-endpoints",
            "Static intensity at both endpoints of the device range (A=0, B=200) with no \
             input at all. Static values bypass the engines and range scaling entirely.",
            chan(cfg_default_params(stat(0.0))),
            chan(cfg_default_params(stat(200.0))),
        )
        .ticks(5)
        .build(),
    );

    out.push(
        SpecBuilder::new(
            "static-intensity-midpoint-and-overflow",
            "Static intensity mid-scale (A=100) and above the device maximum (B=255). The \
             overflow case pins the 200 clamp in the static branch of get_next_waveform_data.",
            chan(cfg_default_params(stat(100.0))),
            chan(cfg_default_params(stat(255.0))),
        )
        .ticks(5)
        .build(),
    );

    // --- Linked, linear + inverse across the full axis range --------------
    out.push(
        SpecBuilder::new(
            "linked-linear-full-axis-sweep",
            "Both channels linked over the full 0..200 range with a linear curve, driven by a \
             slow 0.0→1.0 sweep on L0 (A) and R2 (B) at 20ms cadence. Covers the whole axis \
             domain through the V2Sustained downsampler.",
            chan(cfg_default_params(link("L0", 0.0, 200.0, CurveType::Linear))),
            chan(cfg_default_params(link("R2", 0.0, 200.0, CurveType::Linear))),
        )
        .ticks(14)
        .sweep(0, 20, 61, "L0", 0.0, 1.0)
        .sweep(0, 20, 61, "R2", 0.0, 1.0)
        .build(),
    );

    out.push(
        SpecBuilder::new(
            "linked-inverse-full-axis-sweep",
            "Same sweep as linked-linear-full-axis-sweep but with the inverse curve (1-x) on \
             both channels, so output falls as the axis rises.",
            chan(cfg_default_params(link("L0", 0.0, 200.0, CurveType::Inverse))),
            chan(cfg_default_params(link("R2", 0.0, 200.0, CurveType::Inverse))),
        )
        .ticks(14)
        .sweep(0, 20, 61, "L0", 0.0, 1.0)
        .sweep(0, 20, 61, "R2", 0.0, 1.0)
        .build(),
    );

    out.push(
        SpecBuilder::new(
            "linked-linear-vs-inverse-same-axis",
            "The reference config from the V1 scope doc: both channels driven by L0, A linear \
             and B inverse, so the two channels mirror each other.",
            chan(cfg_default_params(link("L0", 0.0, 200.0, CurveType::Linear))),
            chan(cfg_default_params(link("L0", 0.0, 200.0, CurveType::Inverse))),
        )
        .ticks(12)
        .sweep(0, 25, 45, "L0", 0.0, 1.0)
        .build(),
    );

    // --- V2 ramp (interval_ms) -------------------------------------------
    //
    // These are the only fixtures whose input events carry `interval_ms`.
    // Without one, `apply_tcode` passes `duration_ms = 0` to
    // `V2ChannelState::set_target`, `ramp_end_time == ramp_start_time`, and
    // `get_value_at` returns the target on its very first branch — the
    // interpolation body never executes. A port that implements
    // `getValueAt()` as `return targetValue` would otherwise pass every
    // other fixture here while turning every ramped command into an instant
    // jump on hardware.
    out.push(
        SpecBuilder::new(
            "ramped-targets",
            "Sparse T-Code commands carrying I<ms> ramp durations (600ms up, 500ms down) with \
             long gaps between them. While a ramp is in flight `Channel::next_raw_values` reads \
             V2's interpolation over [now-100, now-25] and ignores the downsampler, so each \
             ramp is monotonic from the tick the command lands to the tick it completes. \
             \
             This is the fixture that catches a port dispatching the other way round. \
             `apply_tcode` also stores the commanded TARGET in the downsampler; letting that \
             win — as the engine used to — put the tick containing the command straight at the \
             endpoint and then read back DOWN off the ramp for the next two. A 600ms ease to \
             full reached full scale after ~100ms and held it, and the ramp ran inverted for \
             three ticks. The same dispatch dropped ramp-downs early: +1500 read [0,0,0,0] \
             400ms before the commanded stop instead of [200,190,180,170].",
            chan(cfg_default_params(link("L0", 0.0, 200.0, CurveType::Linear))),
            chan(cfg_default_params(link("R2", 0.0, 200.0, CurveType::Linear))),
        )
        .ticks(24)
        .at(0, "L0", 0.0)
        .at(0, "R2", 0.0)
        .at_ramp(200, "L0", 1.0, 600)
        .at_ramp(1400, "L0", 0.0, 500)
        .at_ramp(200, "R2", 0.6, 800)
        .at_ramp(1400, "R2", 0.2, 300)
        .build(),
    );

    out.push(
        SpecBuilder::new(
            "ramp-retarget-midflight",
            "A new ramped target arrives while the previous ramp is still in flight, exercising \
             `ramp_start_value = self.get_value_at(timestamp)` in V2ChannelState::set_target \
             against a partially-completed ramp. A: 0 -> 1.0 over 800ms, retargeted to 0.3 over \
             400ms at +600ms (mid-ramp), then to 0.9 over 600ms. B: the same pattern with an \
             inverse curve and shorter ramps. \
             \
             Two re-anchor mistakes are worth distinguishing. Anchoring from the PREVIOUS TARGET \
             is caught only here — no other trace retargets mid-ramp. Anchoring from \
             `current_value` is caught here AND by ramped-targets, where `current_value` is only \
             ever assigned for the `duration_ms == 0` command at t=0 and so stays 0 for the whole \
             trace; that port ramps 0 -> 0 at +1400 and emits flat zeros where the fixture has \
             [160,150,140,130]. \
             \
             Note also tick +600: all four sample points fall at or before the new \
             `ramp_start_time`, so `get_value_at` returns the anchor itself and the re-anchor \
             value appears in the output as a bare [100,100,100,100]. A port that computes it \
             wrong fails both on that literal and on the interpolated ramp that follows. \
             \
             Tick +700 is the retarget's own overshoot check: the new target's sample sits in \
             that window's downsampler, and a port that prefers the downsampler over an \
             in-flight ramp emits a flat [60,60,60,60] — the endpoint of a 400ms ease, 300ms \
             early — where the ramp gives [100,98,95,93].",
            chan(cfg_default_params(link("L0", 0.0, 200.0, CurveType::Linear))),
            chan(cfg_default_params(link("R2", 0.0, 200.0, CurveType::Inverse))),
        )
        .ticks(24)
        .at(0, "L0", 0.0)
        .at(0, "R2", 1.0)
        .at_ramp(200, "L0", 1.0, 800)
        .at_ramp(600, "L0", 0.3, 400)
        .at_ramp(1200, "L0", 0.9, 600)
        .at_ramp(200, "R2", 0.1, 500)
        .at_ramp(450, "R2", 0.8, 300)
        .at_ramp(1200, "R2", 0.4, 400)
        .build(),
    );

    // --- Oscillation threshold -------------------------------------------
    out.push(
        SpecBuilder::new(
            "oscillation-threshold-boundary",
            "Straddles downsample_dynamic's OSCILLATION_THRESHOLD of 40 device units, which \
             switches the algorithm outright: `range < 40` falls back to peak-preserving \
             forward-fill, `range >= 40` emits the alternating min/max pattern. Each channel \
             steps through three 500ms phases whose per-window spread is exactly 39, 40 and 41 \
             device units (axis pairs 0.100/0.295, 0.100/0.300, 0.100/0.305 -> 20/59, 20/60, \
             20/61), and the two channels run the phases in opposite order so both branches are \
             live in the same tick. Phase edges land on tick boundaries so no downsampler window \
             ever mixes two phases. \
             \
             Which mistake each phase catches: a threshold transcribed as 30 diverges on the 39 \
             phase; as 50, on both the 40 and 41 phases; `<` written as `<=` diverges on the 40 \
             phase alone. \
             \
             DO NOT TRIM TICKS FROM THIS FIXTURE. The two branches produce IDENTICAL output at \
             ticks +600, +800 and +1000, where the window's sample parity starts low. The entire \
             discriminating power sits in the ticks where parity starts high: +700 and +900 on \
             both channels (the 40 phase), plus +200/+400 on B and +1200/+1400 on A (the 41 \
             phase). Delete those and the fixture still looks reasonable, still passes, and \
             tests nothing.",
            chan(cfg_default_params(link("L0", 0.0, 200.0, CurveType::Linear))),
            chan(cfg_default_params(link("R2", 0.0, 200.0, CurveType::Linear))),
        )
        .ticks(16)
        .phased_flick(0, 500, 20, "L0", 0.100, 0.295)
        .phased_flick(500, 1000, 20, "L0", 0.100, 0.300)
        .phased_flick(1000, 1500, 20, "L0", 0.100, 0.305)
        .phased_flick(0, 500, 20, "R2", 0.100, 0.305)
        .phased_flick(500, 1000, 20, "R2", 0.100, 0.300)
        .phased_flick(1000, 1500, 20, "R2", 0.100, 0.295)
        .build(),
    );

    // --- Range clamping ---------------------------------------------------
    out.push(
        SpecBuilder::new(
            "range-partial-window",
            "Linked intensity mapped into a narrow sub-range (A: 50..150, B: 120..140) while \
             the axis sweeps the full 0..1. Pins scale_intensity's min + intensity*range/200 \
             arithmetic and its rounding.",
            chan(cfg_default_params(link("L0", 50.0, 150.0, CurveType::Linear))),
            chan(cfg_default_params(link("R2", 120.0, 140.0, CurveType::Linear))),
        )
        .ticks(12)
        .sweep(0, 20, 51, "L0", 0.0, 1.0)
        .sweep(0, 20, 51, "R2", 0.0, 1.0)
        .build(),
    );

    out.push(
        SpecBuilder::new(
            "range-inverted",
            "range_min > range_max on both channels (A: 200..0, B: 150..50). A transposed range \
             is treated as a data-entry mistake, not a request to invert: both \
             device.rs::scale_intensity and ParameterLinkConfig::ordered_range sort the \
             endpoints, so 200..0 behaves exactly like 0..200 and 150..50 like 50..150. Output \
             rises with input and a resting axis sits at the BOTTOM of the range. \
             \
             This fixture previously captured the opposite: scale_intensity returned `min` \
             verbatim whenever max <= min, so channel A emitted 200 — device maximum — from the \
             first tick with the axis at rest, and did so for every input. Use `curve: inverse` \
             to invert; transposing the endpoints does not.",
            chan(cfg_default_params(link("L0", 200.0, 0.0, CurveType::Linear))),
            chan(cfg_default_params(link("R2", 150.0, 50.0, CurveType::Linear))),
        )
        .ticks(10)
        .sweep(0, 25, 37, "L0", 0.0, 1.0)
        .sweep(0, 25, 37, "R2", 1.0, 0.0)
        .build(),
    );

    out.push(
        SpecBuilder::new(
            "range-degenerate-equal",
            "range_min == range_max (A: 120..120, B: 0..0). A zero-width range is a legitimate \
             way to hold a channel at a constant, so output pins at the shared value for every \
             input. Unaffected by the transposed-range fix — ordering the endpoints of an \
             already-equal pair is a no-op, and this trace is byte-identical across it.",
            chan(cfg_default_params(link("L0", 120.0, 120.0, CurveType::Linear))),
            chan(cfg_default_params(link("R2", 0.0, 0.0, CurveType::Linear))),
        )
        .ticks(8)
        .sweep(0, 25, 29, "L0", 0.0, 1.0)
        .sweep(0, 25, 29, "R2", 0.0, 1.0)
        .build(),
    );

    // --- Soft cap ---------------------------------------------------------
    out.push(
        SpecBuilder::new(
            "intensity-soft-cap",
            "Full-range linked intensity on both channels with the per-channel soft cap set to \
             A=80, B=200. A's intensity byte flat-tops at 80 while B runs free; the waveform \
             bytes are unaffected by the cap.",
            chan_capped(cfg_default_params(link("L0", 0.0, 200.0, CurveType::Linear)), 80),
            chan_capped(cfg_default_params(link("R2", 0.0, 200.0, CurveType::Linear)), 200),
        )
        .ticks(12)
        .sweep(0, 20, 51, "L0", 0.0, 1.0)
        .sweep(0, 20, 51, "R2", 0.0, 1.0)
        .build(),
    );

    // --- V2Sustained peak-hold -------------------------------------------
    out.push(
        SpecBuilder::new(
            "sustained-single-transient",
            "One 20ms full-scale spike on L0 surrounded by silence, then 1.5s of nothing. \
             Isolates IntensityPeakHold: the master intensity byte must stay at the peak for \
             200ms after the spike and then fall back. Channel B is static so the spike's \
             effect on A is unambiguous.",
            chan(cfg_default_params(link("L0", 0.0, 200.0, CurveType::Linear))),
            chan(cfg_default_params(stat(0.0))),
        )
        .ticks(16)
        .at(0, "L0", 0.0)
        .at(320, "L0", 1.0)
        .at(340, "L0", 0.0)
        .build(),
    );

    out.push(
        SpecBuilder::new(
            "sustained-fast-flicking",
            "Rapid pole-flicking: L0 alternates 0.0/1.0 every 10ms for 600ms, then stops dead \
             for 900ms. This is the input pattern V2Sustained was built for — the per-slot \
             waveform keeps oscillating while the master intensity is held at the rolling \
             200ms peak, and the tail shows the hold decaying once input stops.",
            chan(cfg_default_params(link("L0", 0.0, 200.0, CurveType::Linear))),
            chan(cfg_default_params(link("L0", 0.0, 200.0, CurveType::Inverse))),
        )
        .ticks(16)
        .flick(0, 10, 61, "L0", 0.0, 1.0)
        .build(),
    );

    out.push(
        SpecBuilder::new(
            "sustained-step-changes",
            "Step ladder on L0: 0 → 1.0 (held 400ms) → 0.0 (held 500ms) → 0.5 (held 400ms) → 0. \
             Each falling edge must be followed by exactly 200ms of peak hold before the master \
             intensity drops, which at a 100ms tick is observable across two ticks.",
            chan(cfg_default_params(link("L0", 0.0, 200.0, CurveType::Linear))),
            chan(cfg_default_params(link("L0", 0.0, 200.0, CurveType::Linear))),
        )
        .ticks(20)
        .at(0, "L0", 0.0)
        .at(200, "L0", 1.0)
        .at(600, "L0", 0.0)
        .at(1100, "L0", 0.5)
        .at(1500, "L0", 0.0)
        .build(),
    );

    out.push(
        SpecBuilder::new(
            "sustained-peak-hold-boundary",
            "Two isolated 10ms full-scale pulses (+150ms, +550ms) landing mid-tick rather than on \
             a tick boundary, so the peak enters the hold buffer from a tick whose own slot \
             values then immediately return to zero. \
             \
             What this does NOT do, despite the pulse offsets looking deliberate: it does not \
             move the hold-window boundary. `IntensityPeakHold::observe` is called exactly once \
             per tick with the tick instant, so every sample in the buffer is a tick timestamp at \
             100ms spacing, and `peak_in_last_ms(now, 200)` with its `>=` cutoff therefore always \
             spans exactly the current tick plus the two before it. That property is fixed by the \
             tick grid dividing 200ms evenly and holds in every sustained fixture — editing these \
             offsets will not change it. Kept because the mid-tick placement still gives the \
             cleanest single-pulse decay shape of the set.",
            chan(cfg_default_params(link("L0", 0.0, 200.0, CurveType::Linear))),
            chan(cfg_default_params(stat(0.0))),
        )
        .ticks(14)
        .at(0, "L0", 0.0)
        .at(150, "L0", 1.0)
        .at(160, "L0", 0.0)
        .at(550, "L0", 1.0)
        .at(560, "L0", 0.0)
        .build(),
    );

    // --- Frequency --------------------------------------------------------
    out.push(
        SpecBuilder::new(
            "frequency-period-boundaries",
            "Frequency linked to L1 over the protocol range 1..200 Hz, stepped through values \
             chosen to land on every branch boundary of convert_period: period 5/10/50/95/100 \
             (the <=100 passthrough, including its upper edge), 101/200/500/588/600 (the \
             /5+100 branch, including both edges), 602/833/1000 (the /10+200 branch, including \
             its upper edge). Note the final `>1000 -> 240` branch is \
             UNREACHABLE from the device path: the resolver clamps frequency to >=1 Hz, so \
             frequency_to_period never exceeds 1000. Channel B holds 100Hz static as a control.",
            chan(ChannelConfig {
                frequency: link("L1", 1.0, 200.0, CurveType::Linear),
                frequency_balance: stat(128.0),
                intensity_balance: stat(128.0),
                intensity: stat(100.0),
            }),
            chan(cfg_default_params(stat(100.0))),
        )
        .ticks(14)
        .at(0, "L1", axis_for_hz(200.0))
        .at(100, "L1", axis_for_hz(100.0))
        .at(200, "L1", axis_for_hz(20.0))
        .at(300, "L1", axis_for_hz(10.5))
        .at(400, "L1", axis_for_hz(10.0))
        .at(500, "L1", axis_for_hz(9.9))
        .at(600, "L1", axis_for_hz(5.0))
        .at(700, "L1", axis_for_hz(2.0))
        .at(800, "L1", axis_for_hz(1.7))
        .at(900, "L1", axis_for_hz(1.0 / 0.6))
        .at(1000, "L1", axis_for_hz(1.66))
        .at(1100, "L1", axis_for_hz(1.2))
        .at(1200, "L1", axis_for_hz(1.0))
        .at(1300, "L1", 0.0)
        .build(),
    );

    out.push(
        SpecBuilder::new(
            "frequency-per-slot-sweep",
            "Frequency linked to L1 with 25ms-cadence input, so the four B0 period bytes differ \
             within a single tick — this is the sub-100ms frequency sweep get_per_slot_frequencies \
             produces. A uses a linear curve, B inverse, both over 1..200 Hz.",
            chan(ChannelConfig {
                frequency: link("L1", 1.0, 200.0, CurveType::Linear),
                frequency_balance: stat(128.0),
                intensity_balance: stat(128.0),
                intensity: stat(120.0),
            }),
            chan(ChannelConfig {
                frequency: link("L1", 1.0, 200.0, CurveType::Inverse),
                frequency_balance: stat(128.0),
                intensity_balance: stat(128.0),
                intensity: stat(120.0),
            }),
        )
        .ticks(12)
        .sweep(0, 25, 45, "L1", 0.0, 1.0)
        .build(),
    );

    out.push(
        SpecBuilder::new(
            "balance-linked",
            "Frequency balance and intensity balance linked over their full 0..255 range on A \
             (linear, axis R0) and B (inverse, axis R1). These feed the BF command rather than \
             B0, so the B0 bytes stay flat while `freq_balance` / `int_balance` move.",
            chan(ChannelConfig {
                frequency: stat(100.0),
                frequency_balance: link("R0", 0.0, 255.0, CurveType::Linear),
                intensity_balance: link("R0", 0.0, 255.0, CurveType::Inverse),
                intensity: stat(100.0),
            }),
            chan(ChannelConfig {
                frequency: stat(100.0),
                frequency_balance: link("R1", 0.0, 255.0, CurveType::Inverse),
                intensity_balance: link("R1", 0.0, 255.0, CurveType::Linear),
                intensity: stat(100.0),
            }),
        )
        .ticks(10)
        .sweep(0, 25, 37, "R0", 0.0, 1.0)
        .sweep(0, 25, 37, "R1", 1.0, 0.0)
        .build(),
    );

    // --- Midpoint ---------------------------------------------------------
    out.push({
        let mut a_int = link("L0", 0.0, 200.0, CurveType::Linear);
        a_int.midpoint = Some(true);
        let mut b_int = link("L0", 0.0, 200.0, CurveType::Linear);
        b_int.midpoint = Some(false);
        SpecBuilder::new(
            "midpoint-distance-from-center",
            "Channel A has midpoint enabled (|x-0.5|*2 applied before the curve), B does not, \
             both driven by the same L0 sweep 0→1. A traces a V: full at the extremes, zero at \
             centre.",
            chan(cfg_default_params(a_int)),
            chan(cfg_default_params(b_int)),
        )
        .ticks(12)
        .sweep(0, 25, 45, "L0", 0.0, 1.0)
        .build()
    });

    out.push({
        // Intensity carries the composition on A, frequency on B. Those are
        // the two INDEPENDENT sites where midpoint and curve compose:
        // `Channel::apply_tcode` (intensity only — it never touches the
        // resolver) and `resolve_link_at_time` (frequency and both balances).
        // A port can get one right and the other wrong.
        let mut a_int = link("L0", 0.0, 200.0, CurveType::Inverse);
        a_int.midpoint = Some(true);
        let mut b_freq = link("L0", 1.0, 200.0, CurveType::Inverse);
        b_freq.midpoint = Some(true);

        SpecBuilder::new(
            "midpoint-composed-with-inverse",
            "Pins the ORDER of midpoint and curve, which `midpoint-distance-from-center` cannot: \
             it uses a linear curve, where the two orders are indistinguishable. \
             \
             Composed with `inverse` they disagree everywhere, and maximally. Because midpoint is \
             symmetric about 0.5 (|x-0.5|*2 == |(1-x)-0.5|*2), swapping the order does not perturb \
             the output — it INVERTS it. Correct is midpoint-then-curve: 1 - |x-0.5|*2, a tent \
             peaking at x=0.5. Swapped is curve-then-midpoint, which collapses to |x-0.5|*2, a V. \
             \
             The safety case is the axis at rest. x=0.0 -> correct gives normalized 0.0, so \
             B0[2] = 0 and B0[12..16] = 240 (1 Hz): silence. Swapped gives 1.0, so B0[2] = 200 \
             and the period bytes drop to 5 (200 Hz): FULL DEVICE OUTPUT WITH THE AXIS PARKED AT \
             ZERO. That is the failure this whole fixture corpus exists to prevent. \
             \
             The trace parks at 0.0 for four ticks, then steps 0.5 (tent peak, 200 vs 0 — the \
             mirror-image divergence), 0.1, 0.9, 1.0, and returns to rest at 0.0. \
             \
             NOTE x=0.25 and x=0.75 are the blind spot: midpoint maps both to 0.5, and 1-0.5 == \
             0.5, so the two orders agree there. They are included to document that, not to \
             discriminate. Every OTHER value in this trace is load-bearing — do not reduce it to \
             the quarter points.",
            chan(cfg_default_params(a_int)),
            chan(ChannelConfig {
                frequency: b_freq,
                frequency_balance: stat(128.0),
                intensity_balance: stat(128.0),
                // Static so B's intensity byte stays constant and the
                // frequency bytes carry the resolver-site signal alone.
                intensity: stat(100.0),
            }),
        )
        .ticks(23)
        .at(0, "L0", 0.0)
        .at(400, "L0", 0.5)
        .at(700, "L0", 0.25)
        .at(1000, "L0", 0.1)
        .at(1300, "L0", 0.75)
        .at(1500, "L0", 0.9)
        .at(1700, "L0", 1.0)
        .at(1900, "L0", 0.0)
        .build()
    });

    // --- Stalled device loop / intensity replay floor ---------------------
    out.push(
        SpecBuilder::new(
            "tick-gap-replay-floor",
            "The only trace with a non-uniform tick timeline (see `tick_offsets_ms`). A real 10Hz \
             loop misses deadlines when the tab or app is backgrounded, and one engine behaviour \
             is reachable only through such a gap: INTENSITY_REPLAY_FLOOR_MS. \
             \
             `replay_pending_intensity_samples` feeds axis samples in (after, now], where \
             `after = max(watermark, now - 200)`. The 200ms floor stops a stale watermark \
             dragging ancient history into the engine after a stall. Every other fixture ticks \
             every 100ms, so the watermark is never more than 100ms behind and the floor never \
             binds — it is unobservable across the entire rest of the corpus. \
             \
             Ticks run 0,100,200,300 then JUMP to 900 (a 600ms stall) before resuming at 100ms. \
             Both channels sit at 0.3 (intensity 60) before the stall, and both receive one \
             sample during it, placed on opposite sides of the floor: \
             \
             A's arrives at +350 — that is 550ms before the resumed tick, outside the 200ms \
             floor, so it is DROPPED and A must still read 60 at +900. \
             B's arrives at +750 — 150ms before, inside the floor, so it IS replayed and B must \
             read 180 at +900. \
             \
             Same stall, same timeline, opposite outcomes, so the fixture pins the floor's \
             BOUNDARY rather than merely its existence. A port with no floor replays both and \
             emits 200 for A (the axis was commanded to 1.0). A port that discards everything \
             after any gap replays neither and emits 60 for B. Both mistakes are caught, and A's \
             is the dangerous direction. \
             \
             Input resumes normally at +1150 so the trace also shows the channel is not wedged \
             by the drop.",
            chan(cfg_default_params(link("L0", 0.0, 200.0, CurveType::Linear))),
            chan(cfg_default_params(link("R2", 0.0, 200.0, CurveType::Linear))),
        )
        .tick_offsets(vec![0, 100, 200, 300, 900, 1000, 1100, 1200, 1300, 1400])
        .at(0, "L0", 0.3)
        .at(0, "R2", 0.3)
        .at(350, "L0", 1.0)
        .at(750, "R2", 0.9)
        .at(1150, "L0", 0.8)
        .at(1150, "R2", 0.5)
        .build(),
    );

    // --- Channel independence --------------------------------------------
    out.push(
        SpecBuilder::new(
            "channels-independent",
            "Fully asymmetric channels: A linked to L0 (linear, 0..200, cap 200, 100Hz static \
             frequency), B linked to R2 (inverse, 20..180, cap 150, frequency linked to L1). \
             The two channels are driven by unrelated input so any cross-channel state leak \
             shows up immediately.",
            chan(cfg_default_params(link("L0", 0.0, 200.0, CurveType::Linear))),
            chan_capped(
                ChannelConfig {
                    frequency: link("L1", 1.0, 200.0, CurveType::Linear),
                    frequency_balance: stat(200.0),
                    intensity_balance: stat(64.0),
                    intensity: link("R2", 20.0, 180.0, CurveType::Inverse),
                },
                150,
            ),
        )
        .ticks(14)
        .sweep(0, 20, 51, "L0", 0.0, 1.0)
        .flick(0, 40, 26, "R2", 0.2, 0.9)
        .sweep(0, 50, 21, "L1", 0.1, 0.8)
        .build(),
    );

    // --- No-input behaviour ----------------------------------------------
    //
    // `no_input_decay_ms` is 3000, NOT the app default of 1000, purely so the
    // ramp spans enough ticks to be read by eye. Decay now works at any
    // `decay_ms`: it measures progress from the instant the axis goes stale
    // (`STALENESS_THRESHOLD_MS`), not from the last sample, so the ramp starts
    // at the held value and reaches zero `decay_ms` later. Timing it from the
    // sample — as this used to — spent the whole staleness threshold before
    // Decay was even reachable, which made it an exact alias for Zero at the
    // shipped default of 1000.
    //
    // Every linked parameter carries `staticValue` so the Default branch has
    // something distinguishable to fall back to. Note the semantic trap this
    // exposes: for a LINKED parameter, `handle_no_input` returns
    // `static_value` as the *raw, pre-curve, normalized 0..1 input*, which
    // then runs through the curve and range mapping. On a Static parameter
    // the same field is the device value verbatim. The frequency link below
    // uses 0.75, which resolves to lerp(1, 200, 0.75) = 150.25 Hz, not 0.75.
    const DECAY_MS: u32 = 3000;
    for (behavior, slug, blurb) in [
        (
            NoInputBehavior::Hold,
            "hold",
            "the last value is held indefinitely",
        ),
        (
            NoInputBehavior::Zero,
            "zero",
            "the value snaps to zero the moment the axis goes stale",
        ),
        (
            NoInputBehavior::Decay,
            "decay",
            "the value leaves the held level at the staleness threshold and ramps to zero over \
             no_input_decay_ms from there, reaching zero at age_ms = 1000 + 3000 (tick 43 here)",
        ),
        (
            NoInputBehavior::Default,
            "default",
            "the value falls back to the link's staticValue (0.75 pre-curve on the frequency \
             link, so 150.25 Hz)",
        ),
    ] {
        let mut freq = link("L1", 1.0, 200.0, CurveType::Linear);
        freq.static_value = Some(0.75);
        let mut int_a = link("L0", 0.0, 200.0, CurveType::Linear);
        int_a.static_value = Some(0.4);
        let mut int_b = link("L0", 0.0, 200.0, CurveType::Inverse);
        int_b.static_value = Some(0.4);

        out.push(
            SpecBuilder::new(
                &format!("no-input-{}", slug),
                &format!(
                    "Input on L0 and L1 stops after 300ms and the trace runs on for another 4.2s, \
                     crossing the resolver's 1000ms staleness threshold at tick 14 and the end of \
                     the decay window at tick 43. With no_input_behavior = {}, {}. Frequency \
                     (linked to L1) shows the effect directly in the B0 period bytes. Intensity \
                     is on the ENGINE path, which holds via the V2 ramp regardless of this \
                     setting — no_input_behavior does not reach it, so the intensity bytes are \
                     identical across all four of these fixtures by design.",
                    slug, blurb
                ),
                chan(ChannelConfig {
                    frequency: freq,
                    frequency_balance: stat(128.0),
                    intensity_balance: stat(128.0),
                    intensity: int_a,
                }),
                chan(cfg_default_params(int_b)),
            )
            // 46 ticks so the trace outlasts the decay window (which now ends
            // at STALENESS_THRESHOLD_MS + DECAY_MS after the last sample) and
            // shows the floor being held afterwards.
            .ticks(46)
            .no_input(behavior, DECAY_MS)
            .sweep(0, 50, 7, "L0", 0.2, 0.8)
            .sweep(0, 50, 7, "L1", 0.1, 0.9)
            .build(),
        );
    }

    out.push(
        SpecBuilder::new(
            "no-input-at-all",
            "Every parameter linked, zero input events for the whole trace. Exercises \
             handle_no_input_no_state (axis slot never created) and the engines' cold-start \
             output. Should be all zeros with the frequency floor at 1 Hz.",
            chan(ChannelConfig {
                frequency: link("L1", 1.0, 200.0, CurveType::Linear),
                frequency_balance: link("R0", 0.0, 255.0, CurveType::Linear),
                intensity_balance: link("R1", 0.0, 255.0, CurveType::Linear),
                intensity: link("L0", 0.0, 200.0, CurveType::Linear),
            }),
            chan(ChannelConfig {
                frequency: link("L1", 1.0, 200.0, CurveType::Inverse),
                frequency_balance: link("R0", 0.0, 255.0, CurveType::Inverse),
                intensity_balance: link("R1", 0.0, 255.0, CurveType::Inverse),
                intensity: link("R2", 0.0, 200.0, CurveType::Inverse),
            }),
        )
        .ticks(6)
        .build(),
    );

    out
}

// ============================================================================
// Emission
// ============================================================================

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("docs")
        .join("spikes")
        .join("pwa")
        .join("fixtures")
}

fn build_fixture(spec: TraceSpec) -> Fixture {
    let ticks = run_trace(&spec);
    Fixture {
        schema_version: SCHEMA_VERSION,
        spec,
        ticks,
    }
}

#[derive(Serialize)]
struct IndexEntry {
    name: String,
    file: String,
    description: String,
    tick_count: usize,
}

#[derive(Serialize)]
struct Index {
    schema_version: u32,
    /// `V2Sustained` for every fixture — the only engine in V1 scope.
    engine: ProcessingEngineType,
    /// The all-stop B0 frame `device.rs::send_zero_command` writes on pause.
    zero_b0: Vec<u8>,
    fixtures: Vec<IndexEntry>,
}

/// Serialize with a trailing newline so the files are diff-friendly.
fn to_json(value: &impl Serialize) -> String {
    let mut s = serde_json::to_string_pretty(value).expect("fixture serializes");
    s.push('\n');
    s
}

/// Render the whole fixture set to `(relative_path, contents)` pairs. Pure —
/// no filesystem access — so the determinism test can compare two runs
/// without touching disk.
fn render_all() -> Vec<(String, String)> {
    let mut files = Vec::new();
    let mut index = Vec::new();

    for spec in all_specs() {
        let file = format!("{}.json", spec.name);
        let fixture = build_fixture(spec);
        index.push(IndexEntry {
            name: fixture.spec.name.clone(),
            file: file.clone(),
            description: fixture.spec.description.clone(),
            tick_count: fixture.ticks.len(),
        });
        files.push((file, to_json(&fixture)));
    }

    files.push((
        "index.json".to_string(),
        to_json(&Index {
            schema_version: SCHEMA_VERSION,
            engine: ProcessingEngineType::V2Sustained,
            zero_b0: build_zero_b0_frame(),
            fixtures: index,
        }),
    ));

    files
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regenerate every fixture into `docs/spikes/pwa/fixtures/`.
    ///
    /// Regeneration command:
    /// ```text
    /// cargo test --manifest-path src-tauri/Cargo.toml golden -- --nocapture
    /// ```
    /// (or `npm run fixtures`)
    ///
    /// Generation is deterministic, so re-running against an unchanged engine
    /// leaves an empty git diff. A non-empty diff means the engine's
    /// behaviour changed.
    #[test]
    fn generate_golden_fixtures() {
        let dir = fixtures_dir();
        std::fs::create_dir_all(&dir).expect("create fixtures dir");

        let files = render_all();
        for (name, contents) in &files {
            std::fs::write(dir.join(name), contents).expect("write fixture");
        }
        println!(
            "wrote {} files to {}",
            files.len(),
            dir.display()
        );
    }

    /// Two independent runs of the generator must produce byte-identical
    /// output. Guards against clock reads, `HashMap` iteration order, or any
    /// other ambient state creeping into the trace path.
    #[test]
    fn golden_fixtures_are_deterministic() {
        let first = render_all();
        let second = render_all();
        assert_eq!(first.len(), second.len());
        for ((n1, c1), (n2, c2)) in first.iter().zip(second.iter()) {
            assert_eq!(n1, n2, "file order diverged");
            assert_eq!(
                c1.as_bytes(),
                c2.as_bytes(),
                "fixture {} is not deterministic",
                n1
            );
        }
    }

    /// Every B0 frame is exactly 20 bytes and starts with the 0xB0 header
    /// plus the fixed 0b1111 interpretation byte. Cheap structural guard so a
    /// malformed fixture can't be committed silently.
    #[test]
    fn golden_b0_frames_are_well_formed() {
        for spec in all_specs() {
            let name = spec.name.clone();
            for tick in run_trace(&spec) {
                assert_eq!(tick.b0.len(), 20, "{}: B0 must be 20 bytes", name);
                assert_eq!(tick.b0[0], 0xB0, "{}: B0 header", name);
                assert_eq!(tick.b0[1], 0x0F, "{}: interpretation byte", name);
                assert!(tick.b0[2] <= 200 && tick.b0[3] <= 200, "{}: intensity", name);
            }
        }
    }

    /// The ramp fixtures must actually drive `V2ChannelState`'s interpolation
    /// body. Without an `interval_ms`, `set_target` collapses the ramp and
    /// `get_value_at` short-circuits on its first branch — a port could then
    /// implement `getValueAt` as `return target` and pass everything. Assert
    /// both that ramped input exists and that it produces at least one tick
    /// whose four slots are strictly increasing mid-ramp values, which only
    /// the interpolation branch can generate.
    #[test]
    fn ramp_fixtures_exercise_v2_interpolation() {
        let mut ramped_events = 0usize;
        let mut saw_interpolated_tick = false;

        for spec in all_specs() {
            ramped_events += spec.inputs.iter().filter(|e| e.interval_ms.is_some()).count();
            if !spec.name.starts_with("ramp") {
                continue;
            }
            for tick in run_trace(&spec) {
                for ch in [&tick.channel_a, &tick.channel_b] {
                    let v = ch.raw_values;
                    let strictly_rising = v[0] < v[1] && v[1] < v[2] && v[2] < v[3];
                    // A mid-ramp tick: every slot strictly between the ramp
                    // endpoints, so neither a held value nor a settled target.
                    if strictly_rising && v[0] > 0 && v[3] < 200 {
                        saw_interpolated_tick = true;
                    }
                }
            }
        }

        assert!(ramped_events > 0, "no fixture carries interval_ms");
        assert!(
            saw_interpolated_tick,
            "no tick shows V2's ramp interpolation mid-flight"
        );
    }

    /// The peak-hold fixtures must actually exercise the hold: at least one
    /// tick where the master intensity exceeds every per-slot engine value,
    /// which can only happen when `IntensityPeakHold` sustains a past peak.
    #[test]
    fn sustained_fixtures_exercise_peak_hold() {
        for spec in all_specs() {
            if !spec.name.starts_with("sustained-") {
                continue;
            }
            let name = spec.name.clone();
            let ticks = run_trace(&spec);
            let held = ticks.iter().any(|t| {
                let slot_max = *t.channel_a.raw_values.iter().max().unwrap();
                t.channel_a.raw_intensity > slot_max
            });
            assert!(held, "{}: no tick shows the peak-hold sustaining", name);
        }
    }
}
