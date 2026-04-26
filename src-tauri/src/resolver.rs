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
/// in `modulation.rs` plus an explicit `is_static` boolean — the
/// frontend uses that to decide whether to render the curve plot
/// position line at all (Static parameters have no input axis to land
/// on).
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
    /// Bus axis name when Linked; absent for Static. The frontend
    /// uses it to pick which input-monitor card the resolved dot
    /// belongs on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_axis: Option<String>,
    /// `true` when the parameter source is `Static` — UI hides the
    /// curve plot position line in that case.
    pub is_static: bool,
}

impl ResolvedSampleSnapshot {
    fn from_sample(sample: crate::modulation::ResolvedSample, is_static: bool) -> Self {
        Self {
            raw_input: sample.raw_input,
            normalized_pre_range: sample.normalized_pre_range,
            device_value: sample.device_value,
            target_time_ms: sample.target_time_ms,
            source_axis: sample.source_axis,
            is_static,
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
    use crate::modulation::{
        resolve_link, ParameterSourceType, ResolvedSample,
    };

    let state = get_processing_state().await;
    let mut state_guard = state.write().await;
    let now = current_time_ms();
    let no_input_behavior = state_guard.no_input_behavior.clone();
    let decay_ms = state_guard.no_input_decay_ms;
    let engine = state_guard.options.processing_engine;

    let (bus, channels) = state_guard.split_bus_and_channels();

    let resolve_one =
        |ch: &mut crate::processing::Channel|
            -> (ResolvedChannelParams, ChannelResolvedSnapshot) {
            let intensity_is_static =
                ch.config.intensity.source_type == ParameterSourceType::Static;
            let range_min = ch.config.intensity.range_min as u8;
            let range_max = ch.config.intensity.range_max as u8;

            let freq_sample = resolve_link(
                &ch.config.frequency,
                &mut ch.link_runtime.frequency,
                bus,
                &no_input_behavior,
                now,
                decay_ms,
            );
            let freq_bal_sample = resolve_link(
                &ch.config.frequency_balance,
                &mut ch.link_runtime.frequency_balance,
                bus,
                &no_input_behavior,
                now,
                decay_ms,
            );
            let int_bal_sample = resolve_link(
                &ch.config.intensity_balance,
                &mut ch.link_runtime.intensity_balance,
                bus,
                &no_input_behavior,
                now,
                decay_ms,
            );

            // Intensity sample: prefer the bp-path resolver output
            // stashed by `get_next_waveform_data`. For Static and
            // engine-path Linked, synthesize so the UI sees a
            // coherent ResolvedSample for every channel.
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
                // Engine-path Linked (T-Code / gamepad). Read the
                // engine's actual transmitted value as `device_value`
                // (V3 lookahead resolves to `current_position`; V2 reads
                // the ramp at `now`), and re-derive `raw_input` /
                // `normalized_pre_range` from the bus + curve so the UI
                // shows where the curve sees the input. Using the engine
                // output here gives the user a faithful "what the device
                // actually receives" reading, which can lag the resolver
                // view under V2 ramping or V3 lookahead.
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
                // Normalized pre-range: if range_max > range_min,
                // unmap the device value back into [0, 1]. The engine
                // already encodes intensity in 0..200 so this is a
                // simple inverse.
                let normalized_pre_range = (device_value / 200.0).clamp(0.0, 1.0);
                ResolvedSample {
                    raw_input,
                    normalized_pre_range,
                    device_value,
                    target_time_ms,
                    source_axis: axis_name,
                }
            };

            let snapshot = ChannelResolvedSnapshot {
                frequency: ResolvedSampleSnapshot::from_sample(
                    freq_sample.clone(),
                    ch.config.frequency.source_type == ParameterSourceType::Static,
                ),
                frequency_balance: ResolvedSampleSnapshot::from_sample(
                    freq_bal_sample.clone(),
                    ch.config.frequency_balance.source_type == ParameterSourceType::Static,
                ),
                intensity_balance: ResolvedSampleSnapshot::from_sample(
                    int_bal_sample.clone(),
                    ch.config.intensity_balance.source_type == ParameterSourceType::Static,
                ),
                intensity: ResolvedSampleSnapshot::from_sample(
                    intensity_sample,
                    intensity_is_static,
                ),
            };

            let params = ResolvedChannelParams {
                frequency: freq_sample.device_value.clamp(1.0, 200.0),
                freq_balance: freq_bal_sample.device_value.clamp(0.0, 255.0) as u8,
                int_balance: int_bal_sample.device_value.clamp(0.0, 255.0) as u8,
                range_min,
                range_max,
                intensity_is_static,
            };

            (params, snapshot)
        };

    let [a, b] = channels;
    let (params_a, snap_a) = resolve_one(a);
    let (params_b, snap_b) = resolve_one(b);
    ((params_a, params_b), (snap_a, snap_b))
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
            out[i] = resolve_link_at_time(
                &ch.config.frequency,
                &mut ch.link_runtime.frequency,
                bus,
                &no_input_behavior,
                now,
                decay_ms,
                target,
            )
            .device_value
            .clamp(1.0, 200.0);
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
        ChannelConfig, CurveType, NoInputBehavior, ParameterLinkConfig, ParameterLinkRuntime,
        ParameterSourceType, ResolvedSample,
    };
    use crate::processing::{Channel, ChannelId, ProcessingEngineType};
    use crate::transforms::TransformConfig;

    fn build_channel_with_intensity(cfg: ParameterLinkConfig, id: ChannelId) -> Channel {
        let mut ch = Channel::new(id);
        ch.link_runtime.intensity = ParameterLinkRuntime::for_config(&cfg);
        ch.config.intensity = cfg;
        ch
    }

    /// Resolver-side: build a `ChannelResolvedSnapshot` from a single
    /// channel's state without a global processing-state lock. Mirrors
    /// the inner closure in `get_resolved_channel_params` so the
    /// snapshot shape can be tested in isolation.
    fn snapshot_one(
        ch: &mut Channel,
        bus: &InputBus,
        no_input_behavior: &NoInputBehavior,
        decay_ms: u32,
        engine: ProcessingEngineType,
        now: u64,
    ) -> ChannelResolvedSnapshot {
        use crate::modulation::resolve_link;

        let intensity_is_static =
            ch.config.intensity.source_type == ParameterSourceType::Static;
        let freq = resolve_link(
            &ch.config.frequency,
            &mut ch.link_runtime.frequency,
            bus,
            no_input_behavior,
            now,
            decay_ms,
        );
        let freq_bal = resolve_link(
            &ch.config.frequency_balance,
            &mut ch.link_runtime.frequency_balance,
            bus,
            no_input_behavior,
            now,
            decay_ms,
        );
        let int_bal = resolve_link(
            &ch.config.intensity_balance,
            &mut ch.link_runtime.intensity_balance,
            bus,
            no_input_behavior,
            now,
            decay_ms,
        );

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
            let normalized_pre_range = (device_value / 200.0).clamp(0.0, 1.0);
            ResolvedSample {
                raw_input,
                normalized_pre_range,
                device_value,
                target_time_ms,
                source_axis: axis_name,
            }
        };

        ChannelResolvedSnapshot {
            frequency: ResolvedSampleSnapshot::from_sample(
                freq,
                ch.config.frequency.source_type == ParameterSourceType::Static,
            ),
            frequency_balance: ResolvedSampleSnapshot::from_sample(
                freq_bal,
                ch.config.frequency_balance.source_type == ParameterSourceType::Static,
            ),
            intensity_balance: ResolvedSampleSnapshot::from_sample(
                int_bal,
                ch.config.intensity_balance.source_type == ParameterSourceType::Static,
            ),
            intensity: ResolvedSampleSnapshot::from_sample(intensity_sample, intensity_is_static),
        }
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
        assert!(!snap.intensity.is_static);
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
        assert!(snap.intensity.is_static);
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
        assert!(!snap.intensity.is_static);
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
    fn channel_resolved_snapshot_serializes_with_camel_case_fields() {
        // Wire-format pin: the frontend types/modulation.ts will read
        // these fields by name. `is_static` lives on each
        // `ResolvedSampleSnapshot`; serde derives keep the field names
        // verbatim so the JSON shape is `{ raw_input, normalized_pre_range,
        // device_value, target_time_ms, source_axis?, is_static }`.
        let snap = ResolvedSampleSnapshot {
            raw_input: 0.5,
            normalized_pre_range: 0.5,
            device_value: 100.0,
            target_time_ms: 1_000,
            source_axis: Some("L0".into()),
            is_static: false,
        };
        let json = serde_json::to_string(&snap).unwrap();
        for k in [
            "raw_input",
            "normalized_pre_range",
            "device_value",
            "target_time_ms",
            "source_axis",
            "is_static",
        ] {
            assert!(json.contains(&format!("\"{}\"", k)), "missing key {} in {}", k, json);
        }
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
            is_static: true,
        };
        let json = serde_json::to_string(&snap).unwrap();
        assert!(!json.contains("source_axis"), "static snapshot must omit source_axis: {}", json);
        assert!(json.contains("\"is_static\":true"));
    }
}
