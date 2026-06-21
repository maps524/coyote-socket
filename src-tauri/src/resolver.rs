//! Read-side helpers over `ProcessingState` that resolve `ParameterLinkConfig`
//! configs into concrete values for downstream consumers (the device tick,
//! the frontend mirror, the diagnostic capture, etc.).
//!
//! Lifted out of `websocket.rs` — these helpers had nothing to do with the
//! WebSocket layer; they sat there only because the T-Code handler did, and
//! the resolver helpers grew alongside it. They belong next to `modulation`
//! since they're the lazy-resolve mirror of `modulation::resolve_parameter`.

use serde::Serialize;

use crate::modulation::NoInputBehavior;
use crate::processing::{current_time_ms, get_processing_state, ProcessingEngineType, WaveformData};

/// Device-units max for intensity (matches the engine's 0..200 byte
/// encoding in `processing.rs::Channel::next_raw_values`). Used by
/// the engine-path snapshot synthesis to invert the device byte back
/// into a 0..1 normalized value for the curve plot. Named to flag
/// that broadening this synthesis to other parameters (e.g. frequency
/// at 1..200 Hz) would need a different divisor.
const INTENSITY_DEVICE_MAX: f64 = 200.0;

/// Resolved channel parameters for device output.
#[derive(Debug, Clone)]
#[allow(dead_code)] // freq_balance and int_balance reserved for BF command support
pub struct ResolvedChannelParams {
    pub frequency: f64,   // Hz (1-200)
    pub freq_balance: u8, // 0-255
    pub int_balance: u8,  // 0-255
    pub range_min: u8,    // 0-200 (only used for linked intensity)
    pub range_max: u8,    // 0-200 (only used for linked intensity)
    pub intensity_is_static: bool,
}

/// Per-parameter resolved-sample shape projected onto the wire format
/// for sub G's `resolved-update` Tauri event. Mirrors `ResolvedSample`
/// in `modulation.rs`. The frontend distinguishes Static from Linked
/// purely by the presence of `source_axis` (omitted for Static via
/// `skip_serializing_if`); a separate `is_static` boolean used to
/// shadow that contract through G.0 and is gone now that G.1 reads
/// the absence directly.
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedSampleSnapshot {
    /// Pre-curve, pre-transforms input value (from the bus or the
    /// static fallback). For Linked parameters this is the bus axis
    /// read at `target_time_ms`; for Static it equals
    /// `normalized_pre_range`.
    pub raw_input: f64,
    /// Post-curve, post-transforms value clamped to `[0, 1]`. The UI
    /// renders this as the live "where the resolver thinks we are"
    /// dot on the curve plot.
    pub normalized_pre_range: f64,
    /// Final value the device receives (post-range for Linked,
    /// static value as-is for Static).
    pub device_value: f64,
    /// `current_time_ms - delay_ms` for Linked, or the current tick
    /// time for Static. Lets the UI align the input dot with the
    /// delayed bus read.
    pub target_time_ms: u64,
    /// Bus axis name when Linked; absent (key omitted from JSON) for
    /// Static. The frontend treats absence as "no axis to plot" and
    /// hides the position dot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_axis: Option<String>,
}

impl ResolvedSampleSnapshot {
    /// Project a `ResolvedSample` onto the wire-format snapshot.
    /// Consumes the sample so `source_axis: Option<String>` moves
    /// without a clone — the production resolver pass calls this
    /// once per parameter slot per channel per tick (8 calls/sec at
    /// 10Hz), so saving allocations is cheap and worth it.
    fn from_sample(sample: crate::modulation::ResolvedSample) -> Self {
        Self {
            raw_input: sample.raw_input,
            normalized_pre_range: sample.normalized_pre_range,
            device_value: sample.device_value,
            target_time_ms: sample.target_time_ms,
            source_axis: sample.source_axis,
        }
    }
}

/// Per-channel snapshot of all four resolver outputs at one tick.
/// Carried inside `ResolvedUpdatePayload`; one per channel.
#[derive(Debug, Clone, Serialize)]
pub struct ChannelResolvedSnapshot {
    pub frequency: ResolvedSampleSnapshot,
    pub frequency_balance: ResolvedSampleSnapshot,
    pub intensity_balance: ResolvedSampleSnapshot,
    pub intensity: ResolvedSampleSnapshot,
}

/// Wire payload for the `resolved-update` Tauri event. Emitted at the
/// 10Hz device tick from `device.rs::send_device_update` alongside
/// `waveform-sample`. Sub G's frontend `resolvedState.ts` store reads
/// this directly.
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedUpdatePayload {
    pub timestamp_ms: u64,
    pub channel_a: ChannelResolvedSnapshot,
    pub channel_b: ChannelResolvedSnapshot,
}

/// Resolve frequency / freqBalance / intBalance for both channels at the
/// current instant, and project all four parameters' resolver output
/// (including intensity) into per-channel snapshots for the sub G
/// `resolved-update` event.
///
/// Returns the device-output `ResolvedChannelParams` pair plus the
/// telemetry snapshot pair under one write lock. One pass per tick:
///
/// - Frequency / freqBalance / intBalance always run through
///   `resolve_link` here — those parameters never touch the engine
///   path.
/// - Intensity is sourced from `Channel.last_intensity_sample` for
///   `bp:`-routed Linked links (set by `get_next_waveform_data` to
///   avoid re-running the resolver and double-advancing phase /
///   smoothing state), and synthesized in this function for Static
///   and engine-path Linked links. The synthesis path produces a
///   `ResolvedSample` that matches what the resolver *would* compute
///   if it ran — for Static that's a constant; for engine-path
///   Linked it's the same midpoint+curve+transforms pipeline as
///   `resolve_link` would use, since transforms attached to T-Code
///   intensity are intentionally inert in the engine path (sub E
///   gate). The frontend distinguishes "resolver view" from "engine
///   output" by combining this snapshot with the existing
///   `waveform-sample` event.
///
/// Write lock: same justification as the prior single-purpose
/// helper — stateful transforms on non-intensity parameters need
/// monotonic state advancement across ticks.
pub async fn get_resolved_channel_params() -> (
    (ResolvedChannelParams, ResolvedChannelParams),
    (ChannelResolvedSnapshot, ChannelResolvedSnapshot),
) {
    let state = get_processing_state().await;
    let mut state_guard = state.write().await;
    let now = current_time_ms();
    let no_input_behavior = state_guard.no_input_behavior.clone();
    let decay_ms = state_guard.no_input_decay_ms;
    let engine = state_guard.options.processing_engine;

    let (bus, channels) = state_guard.split_bus_and_channels();
    let [a, b] = channels;
    let (params_a, snap_a) =
        build_channel_snapshot(a, bus, &no_input_behavior, decay_ms, engine, now);
    let (params_b, snap_b) =
        build_channel_snapshot(b, bus, &no_input_behavior, decay_ms, engine, now);
    ((params_a, params_b), (snap_a, snap_b))
}

/// Resolve all four parameter slots for one channel and project them
/// into device-output params + a telemetry snapshot. Pure per-channel
/// helper — no global state — so production
/// (`get_resolved_channel_params`) and the in-crate tests share one
/// body. Caller holds whatever lock guards the bus + channel.
fn build_channel_snapshot(
    ch: &mut crate::processing::Channel,
    bus: crate::input_bus::InputBusSnapshot<'_>,
    no_input_behavior: &NoInputBehavior,
    decay_ms: u32,
    engine: ProcessingEngineType,
    now: u64,
) -> (ResolvedChannelParams, ChannelResolvedSnapshot) {
    use crate::modulation::{resolve_link, ParameterSourceType, ResolvedSample};

    let intensity_is_static = ch.config.intensity.source_type == ParameterSourceType::Static;
    let range_min = ch.config.intensity.range_min as u8;
    let range_max = ch.config.intensity.range_max as u8;

    // Frequency: prefer the per-slot pass's stashed sample.
    // `get_per_slot_frequencies` runs first in the device tick and is the
    // authoritative advancer of `link_runtime.frequency` (four chronological
    // sub-slot steps). Re-resolving here would advance a stateful frequency
    // transform a fifth, out-of-order time and desync the snapshot from the
    // value the device receives. The in-crate test helper has no per-slot
    // pass, so fall back to a direct resolve when the stash is empty — same
    // pattern as `last_intensity_sample` below.
    let freq_sample = ch.last_frequency_sample.take().unwrap_or_else(|| {
        resolve_link(
            &ch.config.frequency,
            &mut ch.link_runtime.frequency,
            bus,
            no_input_behavior,
            now,
            decay_ms,
        )
    });
    let freq_bal_sample = resolve_link(
        &ch.config.frequency_balance,
        &mut ch.link_runtime.frequency_balance,
        bus,
        no_input_behavior,
        now,
        decay_ms,
    );
    let int_bal_sample = resolve_link(
        &ch.config.intensity_balance,
        &mut ch.link_runtime.intensity_balance,
        bus,
        no_input_behavior,
        now,
        decay_ms,
    );

    // Snapshot ergonomics: capture the device values we need for
    // ResolvedChannelParams BEFORE moving the samples into
    // ResolvedSampleSnapshot via from_sample (consume-by-value).
    let freq_device = freq_sample.device_value;
    let freq_bal_device = freq_bal_sample.device_value;
    let int_bal_device = int_bal_sample.device_value;

    // Intensity sample: prefer the bp-path resolver output stashed
    // by `get_next_waveform_data`. For Static and engine-path Linked,
    // synthesize so the UI sees a coherent `ResolvedSample` for
    // every channel regardless of routing.
    let intensity_sample = if let Some(stash) = ch.last_intensity_sample.take() {
        stash
    } else if intensity_is_static {
        ResolvedSample {
            raw_input: ch.config.intensity.static_value.unwrap_or(0.0),
            normalized_pre_range: ch.config.intensity.static_value.unwrap_or(0.0),
            device_value: ch.config.intensity.static_value.unwrap_or(0.0),
            target_time_ms: now,
            source_axis: None,
        }
    } else {
        // Engine-path Linked (T-Code / gamepad). Read the engine's
        // actual transmitted value as `device_value` (V3 lookahead
        // resolves to `current_position`; V2 reads the ramp at
        // `now`), and re-derive `raw_input` / `normalized_pre_range`
        // from the bus + curve so the UI shows where the curve sees
        // the input. Using the engine output here gives the user a
        // faithful "what the device actually receives" reading,
        // which can lag the resolver view under V2 ramping or V3
        // lookahead.
        //
        // Invariant: we should not reach this branch for `bp:`-routed
        // Linked links — `processing.rs::get_next_waveform_data` is
        // contracted to stash a `ResolvedSample` on every bp:-routed
        // tick before the snapshot pass runs. A future write-lock-
        // split refactor could break that ordering silently; assert
        // it loudly so the next change of the routing topology
        // catches the regression in tests instead of in field
        // telemetry.
        debug_assert!(
            !ch.config
                .intensity
                .source_axis
                .as_deref()
                .is_some_and(|a| a.starts_with("bp:")),
            "bp:-routed Linked intensity must stash a ResolvedSample before \
             the snapshot pass; see processing.rs::get_next_waveform_data"
        );
        let device_value = match engine {
            ProcessingEngineType::V3Predictive => ch.v3.current_position as f64,
            _ => ch.v2.get_value_at(now) as f64,
        };
        let axis_name = ch.config.intensity.source_axis.clone();
        let delay = ch.config.intensity.delay_ms.unwrap_or(0) as u64;
        let target_time_ms = now.saturating_sub(delay);
        let raw_input = axis_name
            .as_deref()
            .and_then(|a| bus.value_at(a, target_time_ms))
            .unwrap_or(0.0);
        let normalized_pre_range = (device_value / INTENSITY_DEVICE_MAX).clamp(0.0, 1.0);
        ResolvedSample {
            raw_input,
            normalized_pre_range,
            device_value,
            target_time_ms,
            source_axis: axis_name,
        }
    };

    let snapshot = ChannelResolvedSnapshot {
        frequency: ResolvedSampleSnapshot::from_sample(freq_sample),
        frequency_balance: ResolvedSampleSnapshot::from_sample(freq_bal_sample),
        intensity_balance: ResolvedSampleSnapshot::from_sample(int_bal_sample),
        intensity: ResolvedSampleSnapshot::from_sample(intensity_sample),
    };

    let params = ResolvedChannelParams {
        frequency: freq_device.clamp(1.0, 200.0),
        freq_balance: freq_bal_device.clamp(0.0, 255.0) as u8,
        int_balance: int_bal_device.clamp(0.0, 255.0) as u8,
        range_min,
        range_max,
        intensity_is_static,
    };

    (params, snapshot)
}

/// Resolve per-slot frequencies (Hz) for both channels at 25ms intervals
/// inside the current 100ms window. Each slot walks the axis history at
/// `window_start + slot*25` so fast axis motion produces true sub-100ms
/// frequency sweeps instead of four copies of the same value.
///
/// Holds a write lock so per-slot calls thread mutable transform state
/// through `link_runtime.frequency` — when a `Smooth` or `Hold` transform
/// is attached to frequency, each slot advances the same state in
/// chronological order, matching the engine's per-slot semantics.
///
/// Returns `(chan_a_hz, chan_b_hz)` with 4 entries each, already clamped to
/// the protocol range 1-200 Hz. Callers feed these through
/// `frequency_to_period` → `convert_period` for the device command.
pub async fn get_per_slot_frequencies(window_start: u64) -> ([f64; 4], [f64; 4]) {
    use crate::modulation::resolve_link_at_time;

    let state = get_processing_state().await;
    let mut state_guard = state.write().await;
    let now = current_time_ms();
    let no_input_behavior = state_guard.no_input_behavior.clone();
    let decay_ms = state_guard.no_input_decay_ms;

    let (bus, channels) = state_guard.split_bus_and_channels();

    let resolve_slots = |ch: &mut crate::processing::Channel| -> [f64; 4] {
        let mut out = [0.0f64; 4];
        for i in 0..4 {
            let target = window_start + (i as u64) * 25;
            let sample = resolve_link_at_time(
                &ch.config.frequency,
                &mut ch.link_runtime.frequency,
                bus,
                &no_input_behavior,
                now,
                decay_ms,
                target,
            );
            out[i] = sample.device_value.clamp(1.0, 200.0);
            // The latest sub-slot (closest to `now`) is authoritative for
            // the telemetry snapshot and the V2 scalar frequency. Stash it
            // so `build_channel_snapshot` consumes it instead of calling
            // `resolve_link` again — a second, out-of-order advance of a
            // stateful frequency transform's state.
            if i == 3 {
                ch.last_frequency_sample = Some(sample);
            }
        }
        out
    };

    let [a, b] = channels;
    (resolve_slots(a), resolve_slots(b))
}

/// Read every axis value with no-input behavior applied. Stale axes
/// (>1s since last update) honor the configured `no_input_behavior` —
/// duplicates the per-axis logic in `resolve_parameter` because callers
/// here want the raw bus values (e.g. the frontend axis monitor) rather
/// than the curve-shaped output.
pub async fn get_axis_values_from_processing() -> std::collections::HashMap<String, f64> {
    let state = get_processing_state().await;
    let state_guard = state.read().await;
    let now = current_time_ms();
    let stale_threshold_ms = 1000u64;
    let decay_ms = state_guard.no_input_decay_ms as u64;

    state_guard
        .input_bus
        .iter()
        .map(|(k, v)| {
            let age_ms = now.saturating_sub(v.timestamp);

            let value = if age_ms > stale_threshold_ms {
                match state_guard.no_input_behavior {
                    NoInputBehavior::Hold => v.value,
                    NoInputBehavior::Default | NoInputBehavior::Zero => 0.0,
                    NoInputBehavior::Decay => {
                        let decay_progress =
                            ((age_ms - stale_threshold_ms) as f64 / decay_ms as f64).min(1.0);
                        v.value * (1.0 - decay_progress)
                    }
                }
            } else {
                v.value
            };

            (k.clone(), value)
        })
        .collect()
}

/// Read the current intensity values (UI display, normalized 0.0-1.0).
pub async fn get_current_intensities() -> (f64, f64) {
    let state = get_processing_state().await;
    let state_guard = state.read().await;
    state_guard.get_current_intensities()
}

/// Pull the next 100ms waveform data for both channels. Called at 10Hz by
/// the device tick. Mutates engine state (advances V2 ramps, advances V3
/// lookahead buffer, etc.) so it takes a write lock.
pub async fn get_next_waveform_data() -> (WaveformData, WaveformData) {
    let state = get_processing_state().await;
    let mut state_guard = state.write().await;
    state_guard.get_next_waveform_data()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::input_bus::InputBus;
    use crate::modulation::{
        CurveType, NoInputBehavior, ParameterLinkConfig, ParameterLinkRuntime, ResolvedSample,
    };
    use crate::processing::{Channel, ChannelId, ProcessingEngineType};
    use crate::transforms::TransformConfig;

    fn build_channel_with_intensity(cfg: ParameterLinkConfig, id: ChannelId) -> Channel {
        let mut ch = Channel::new(id);
        ch.link_runtime.intensity = ParameterLinkRuntime::for_config(&cfg);
        ch.config.intensity = cfg;
        ch
    }

    /// Tests share the production `build_channel_snapshot` rather
    /// than a parallel implementation — sub G.0 follow-up consolidated
    /// the body so future changes only land in one place.
    fn snapshot_one(
        ch: &mut Channel,
        bus: &InputBus,
        no_input_behavior: &NoInputBehavior,
        decay_ms: u32,
        engine: ProcessingEngineType,
        now: u64,
    ) -> ChannelResolvedSnapshot {
        let (_, snap) =
            build_channel_snapshot(ch, bus, no_input_behavior, decay_ms, engine, now);
        snap
    }

    #[test]
    fn snapshot_uses_stashed_resolved_sample_for_bp_intensity() {
        // The bp:-path resolver runs once inside `get_next_waveform_data`
        // and stashes the resulting `ResolvedSample` on the channel.
        // The snapshot helper consumes the stash rather than re-running
        // the resolver — important for stateful transforms like
        // Vibrate, which would advance their phase a second time per
        // tick if the resolver re-ran for telemetry.
        let cfg = ParameterLinkConfig::linked_source(
            "bp:Position_0",
            0.0,
            200.0,
            CurveType::Linear,
        );
        let mut ch = build_channel_with_intensity(cfg, ChannelId::A);
        ch.last_intensity_sample = Some(ResolvedSample {
            raw_input: 0.42,
            normalized_pre_range: 0.4,
            device_value: 80.0,
            target_time_ms: 1_000,
            source_axis: Some("bp:Position_0".into()),
        });

        let bus = InputBus::new();
        let snap = snapshot_one(
            &mut ch,
            &bus,
            &NoInputBehavior::Hold,
            1000,
            ProcessingEngineType::V2Balanced,
            2_000,
        );
        assert!((snap.intensity.raw_input - 0.42).abs() < 1e-9);
        assert!((snap.intensity.device_value - 80.0).abs() < 1e-9);
        assert_eq!(snap.intensity.target_time_ms, 1_000);
        assert_eq!(snap.intensity.source_axis.as_deref(), Some("bp:Position_0"));
        // Stash consumed.
        assert!(ch.last_intensity_sample.is_none());
    }

    #[test]
    fn snapshot_synthesizes_for_static_intensity() {
        // A Static intensity link has no bus axis; the snapshot's
        // `is_static` flag is the frontend's signal to hide the curve
        // plot position line, and `device_value` is the static byte.
        let cfg = ParameterLinkConfig::static_source(75.0);
        let mut ch = build_channel_with_intensity(cfg, ChannelId::A);
        let bus = InputBus::new();

        let snap = snapshot_one(
            &mut ch,
            &bus,
            &NoInputBehavior::Hold,
            1000,
            ProcessingEngineType::V2Balanced,
            1_000,
        );
        // Static parameters omit `source_axis` from the wire; that
        // absence is the frontend's signal to hide the position dot.
        assert!(snap.intensity.source_axis.is_none());
        assert!((snap.intensity.device_value - 75.0).abs() < 1e-9);
    }

    #[test]
    fn snapshot_synthesizes_from_v2_for_engine_path_linked_intensity() {
        // T-Code-driven Linked intensity goes through the V2 engine.
        // The stash is None, the source axis is non-`bp:`, so the
        // helper reads `device_value` from the V2 ramp at `now` and
        // re-derives `raw_input` from the bus. This is what the device
        // is actually transmitting — the frontend `device-units` bar
        // reads from this snapshot.
        let cfg =
            ParameterLinkConfig::linked_source("L0", 0.0, 200.0, CurveType::Linear);
        let mut ch = build_channel_with_intensity(cfg, ChannelId::A);
        // Prime the V2 ramp at 100/200 (= 0.5 normalized).
        ch.v2.set_target(100, 0, 1_000);

        let mut bus = InputBus::new();
        bus.update("L0", 0.7, 1_000, None);

        let snap = snapshot_one(
            &mut ch,
            &bus,
            &NoInputBehavior::Hold,
            1000,
            ProcessingEngineType::V2Balanced,
            1_000,
        );
        // V2 ramp at t=1000 reads as the target value (no delay), so
        // device_value should be the engine's view (100), not the
        // resolver's (which would also be 0.7 → 140 for this range).
        assert!((snap.intensity.device_value - 100.0).abs() < 1e-9);
        assert!((snap.intensity.raw_input - 0.7).abs() < 1e-9);
        assert_eq!(snap.intensity.source_axis.as_deref(), Some("L0"));
    }

    #[test]
    fn snapshot_does_not_double_advance_vibrate_phase() {
        // Acceptance for sub G: stateful transforms must not advance
        // twice per tick. The bp:-path resolver runs inside
        // `get_next_waveform_data`; the telemetry pass reads the
        // stash. If a future change re-ran the resolver for telemetry,
        // a Vibrate transform's phase would step twice and the
        // observed wobble frequency would double.
        let mut cfg =
            ParameterLinkConfig::linked_source("bp:Position_0", 0.0, 200.0, CurveType::Linear);
        cfg.transforms = vec![TransformConfig::Vibrate {
            speed_axis: "bp:Vibrate_0".into(),
            distance: 0.2,
        }];
        let mut ch = build_channel_with_intensity(cfg, ChannelId::A);
        // Simulate get_next_waveform_data: it would resolve once and
        // stash. Capture the would-be phase by stashing a sample with
        // a known target_time_ms.
        ch.last_intensity_sample = Some(ResolvedSample {
            raw_input: 0.5,
            normalized_pre_range: 0.5,
            device_value: 100.0,
            target_time_ms: 1_000,
            source_axis: Some("bp:Position_0".into()),
        });

        let bus = InputBus::new();
        let _ = snapshot_one(
            &mut ch,
            &bus,
            &NoInputBehavior::Hold,
            1000,
            ProcessingEngineType::V2Balanced,
            1_000,
        );

        // Snapshot consumed the stash; the runtime's intensity slot
        // for the Vibrate transform was NOT touched. (If
        // `snapshot_one` had re-run resolve_link, the Vibrate
        // `last_target` would have moved off zero.)
        assert!(matches!(
            ch.link_runtime.intensity.transform_state[0],
            crate::transforms::TransformState::Vibrate { last_target: 0, .. }
        ));
    }

    #[test]
    fn snapshot_consumes_frequency_stash_without_readvancing_transform() {
        // Frequency is uniquely double-resolved per tick:
        // `get_per_slot_frequencies` advances `link_runtime.frequency` four
        // times (one per 25ms sub-slot) and stashes its latest sample;
        // `build_channel_snapshot` must CONSUME that stash, not call
        // `resolve_link` again. A second, out-of-order resolve at `now`
        // would advance a stateful Smooth/Vibrate frequency transform a
        // fifth time and desync the snapshot from the value the device
        // actually receives. This pins the single-advance contract.
        let mut freq_cfg =
            ParameterLinkConfig::linked_source("L0", 1.0, 200.0, CurveType::Linear);
        freq_cfg.transforms = vec![TransformConfig::Smooth {
            time_constant_ms: 100.0,
        }];
        let mut ch = Channel::new(ChannelId::A);
        ch.link_runtime.frequency = ParameterLinkRuntime::for_config(&freq_cfg);
        ch.config.frequency = freq_cfg;

        // Simulate the per-slot pass having stashed its last sample.
        ch.last_frequency_sample = Some(ResolvedSample {
            raw_input: 0.6,
            normalized_pre_range: 0.6,
            device_value: 120.0,
            target_time_ms: 1_975,
            source_axis: Some("L0".into()),
        });

        // A bus value that would resolve to something very different if a
        // stray re-resolve ran instead of consuming the stash.
        let mut bus = InputBus::new();
        bus.update("L0", 0.9, 2_000, None);

        let (params, snap) = build_channel_snapshot(
            &mut ch,
            &bus,
            &NoInputBehavior::Hold,
            1000,
            ProcessingEngineType::V2Balanced,
            2_000,
        );

        // Device scalar + telemetry both read the stash, not a fresh resolve.
        assert!((params.frequency - 120.0).abs() < 1e-9);
        assert!((snap.frequency.device_value - 120.0).abs() < 1e-9);
        assert_eq!(snap.frequency.target_time_ms, 1_975);
        // Stash consumed.
        assert!(ch.last_frequency_sample.is_none());
        // The Smooth transform state was NOT advanced — `resolve_link`
        // never ran for frequency this pass (last_ts would be non-zero if
        // it had).
        assert!(matches!(
            ch.link_runtime.frequency.transform_state[0],
            crate::transforms::TransformState::Smooth { last_ts: 0, .. }
        ));
    }

    #[test]
    fn channel_resolved_snapshot_serializes_with_snake_case_fields() {
        // Wire-format pin: the frontend types/modulation.ts reads
        // these fields by name. No `#[serde(rename_all)]` attribute
        // on the snapshot types, so JSON keeps the Rust field names
        // verbatim — snake_case. Matches the existing `WaveformSample`
        // wire shape; sub G.1's frontend store reads the same casing.
        let snap = ResolvedSampleSnapshot {
            raw_input: 0.5,
            normalized_pre_range: 0.5,
            device_value: 100.0,
            target_time_ms: 1_000,
            source_axis: Some("L0".into()),
        };
        let json = serde_json::to_string(&snap).unwrap();
        for k in [
            "raw_input",
            "normalized_pre_range",
            "device_value",
            "target_time_ms",
            "source_axis",
        ] {
            assert!(json.contains(&format!("\"{}\"", k)), "missing key {} in {}", k, json);
        }
        // Wire-format pin: G.1 follow-up dropped `is_static`; the
        // frontend infers Static from `source_axis` absence and must
        // not see this stale field reappear.
        assert!(!json.contains("is_static"), "is_static must not appear on wire: {}", json);
    }

    #[test]
    fn snapshot_clears_stale_bp_stash_on_link_to_static_transition() {
        // Regression for sub G.0 reviewer finding: a user toggling a
        // bp:-routed intensity link to Static mid-session must not
        // leak the prior tick's bp: stash into the next telemetry
        // pass. `processing.rs::get_next_waveform_data` clears the
        // stash inside the Static early-return; this test simulates
        // that contract by clearing the stash before the snapshot
        // helper runs (matching the production flow) and asserts the
        // result reports the static value, not a stale bp: sample.
        let cfg = ParameterLinkConfig::static_source(75.0);
        let mut ch = build_channel_with_intensity(cfg, ChannelId::A);
        // Stale bp: stash from the prior tick.
        ch.last_intensity_sample = Some(ResolvedSample {
            raw_input: 0.42,
            normalized_pre_range: 0.4,
            device_value: 80.0,
            target_time_ms: 999,
            source_axis: Some("bp:Position_0".into()),
        });
        // Production-equivalent clear (the Static early-return in
        // get_next_waveform_data does this).
        ch.last_intensity_sample = None;

        let bus = InputBus::new();
        let snap = snapshot_one(
            &mut ch,
            &bus,
            &NoInputBehavior::Hold,
            1000,
            ProcessingEngineType::V2Balanced,
            1_000,
        );
        // The post-clear branch falls into the `intensity_is_static`
        // path; absence of `source_axis` is the frontend's signal.
        assert!(snap.intensity.source_axis.is_none());
        assert!((snap.intensity.device_value - 75.0).abs() < 1e-9);
    }

    #[test]
    fn channel_resolved_snapshot_omits_source_axis_for_static_parameters() {
        // Static parameters carry `source_axis: None` and the field
        // is `skip_serializing_if = "Option::is_none"` so the wire
        // format omits it for Static instead of emitting `null`. The
        // frontend treats absence as "no axis" without a separate
        // null check.
        let snap = ResolvedSampleSnapshot {
            raw_input: 100.0,
            normalized_pre_range: 100.0,
            device_value: 100.0,
            target_time_ms: 1_000,
            source_axis: None,
        };
        let json = serde_json::to_string(&snap).unwrap();
        assert!(!json.contains("source_axis"), "static snapshot must omit source_axis: {}", json);
    }
}
