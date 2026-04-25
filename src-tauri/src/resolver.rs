//! Read-side helpers over `ProcessingState` that resolve `ParameterLinkConfig`
//! configs into concrete values for downstream consumers (the device tick,
//! the frontend mirror, the diagnostic capture, etc.).
//!
//! Lifted out of `websocket.rs` — these helpers had nothing to do with the
//! WebSocket layer; they sat there only because the T-Code handler did, and
//! the resolver helpers grew alongside it. They belong next to `modulation`
//! since they're the lazy-resolve mirror of `modulation::resolve_parameter`.

use crate::modulation::NoInputBehavior;
use crate::processing::{current_time_ms, get_processing_state, WaveformData};

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

/// Resolve frequency / freqBalance / intBalance for both channels at the
/// current instant. Holds a write lock since the unified resolver threads
/// per-tick state through `Channel.link_runtime` — switching to a read
/// lock would require throwaway runtime per call, which would break
/// stateful transforms like `Smooth` or `Hold` attached to non-intensity
/// parameters by silently resetting their state every tick.
pub async fn get_resolved_channel_params() -> (ResolvedChannelParams, ResolvedChannelParams) {
    use crate::modulation::{resolve_link, ParameterSourceType};

    let state = get_processing_state().await;
    let mut state_guard = state.write().await;
    let now = current_time_ms();
    let no_input_behavior = state_guard.no_input_behavior.clone();
    let decay_ms = state_guard.no_input_decay_ms;

    let (bus, channels) = state_guard.split_bus_and_channels();

    let resolve_one = |ch: &mut crate::processing::Channel| -> ResolvedChannelParams {
        let intensity_is_static = ch.config.intensity.source_type == ParameterSourceType::Static;
        let range_min = ch.config.intensity.range_min as u8;
        let range_max = ch.config.intensity.range_max as u8;

        let freq = resolve_link(
            &ch.config.frequency,
            &mut ch.link_runtime.frequency,
            bus,
            &no_input_behavior,
            now,
            decay_ms,
        )
        .device_value;
        let freq_bal = resolve_link(
            &ch.config.frequency_balance,
            &mut ch.link_runtime.frequency_balance,
            bus,
            &no_input_behavior,
            now,
            decay_ms,
        )
        .device_value;
        let int_bal = resolve_link(
            &ch.config.intensity_balance,
            &mut ch.link_runtime.intensity_balance,
            bus,
            &no_input_behavior,
            now,
            decay_ms,
        )
        .device_value;

        ResolvedChannelParams {
            frequency: freq.clamp(1.0, 200.0),
            freq_balance: freq_bal.clamp(0.0, 255.0) as u8,
            int_balance: int_bal.clamp(0.0, 255.0) as u8,
            range_min,
            range_max,
            intensity_is_static,
        }
    };

    let [a, b] = channels;
    (resolve_one(a), resolve_one(b))
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
