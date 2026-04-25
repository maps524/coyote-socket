//! Read-side helpers over `ProcessingState` that resolve `ParameterSource`
//! configs into concrete values for downstream consumers (the device tick,
//! the frontend mirror, the diagnostic capture, etc.).
//!
//! Lifted out of `websocket.rs` — these helpers had nothing to do with the
//! WebSocket layer; they sat there only because the T-Code handler did, and
//! the resolver helpers grew alongside it. They belong next to `modulation`
//! since they're the lazy-resolve mirror of `modulation::resolve_parameter`.

use crate::modulation::NoInputBehavior;
use crate::processing::{current_time_ms, get_processing_state, ChannelId, WaveformData};

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
/// current instant. Closed over `state_guard` once so the read lock is held
/// for the duration of the per-channel work.
pub async fn get_resolved_channel_params() -> (ResolvedChannelParams, ResolvedChannelParams) {
    use crate::modulation::{resolve_parameter, ParameterSourceType};

    let state = get_processing_state().await;
    let state_guard = state.read().await;
    let now = current_time_ms();

    let resolve_one = |id: ChannelId| -> ResolvedChannelParams {
        let ch = state_guard.channel(id);
        let freq = resolve_parameter(
            &ch.config.frequency,
            &state_guard.axis_values,
            &state_guard.no_input_behavior,
            now,
            state_guard.no_input_decay_ms,
        );
        let freq_bal = resolve_parameter(
            &ch.config.frequency_balance,
            &state_guard.axis_values,
            &state_guard.no_input_behavior,
            now,
            state_guard.no_input_decay_ms,
        );
        let int_bal = resolve_parameter(
            &ch.config.intensity_balance,
            &state_guard.axis_values,
            &state_guard.no_input_behavior,
            now,
            state_guard.no_input_decay_ms,
        );
        let intensity_is_static = ch.config.intensity.source_type == ParameterSourceType::Static;
        ResolvedChannelParams {
            frequency: freq.clamp(1.0, 200.0),
            freq_balance: freq_bal.clamp(0.0, 255.0) as u8,
            int_balance: int_bal.clamp(0.0, 255.0) as u8,
            range_min: ch.config.intensity.range_min as u8,
            range_max: ch.config.intensity.range_max as u8,
            intensity_is_static,
        }
    };

    (resolve_one(ChannelId::A), resolve_one(ChannelId::B))
}

/// Resolve per-slot frequencies (Hz) for both channels at 25ms intervals
/// inside the current 100ms window. Each slot walks the axis history at
/// `window_start + slot*25` so fast axis motion produces true sub-100ms
/// frequency sweeps instead of four copies of the same value.
///
/// Returns `(chan_a_hz, chan_b_hz)` with 4 entries each, already clamped to
/// the protocol range 1-200 Hz. Callers feed these through
/// `frequency_to_period` → `convert_period` for the device command.
pub async fn get_per_slot_frequencies(window_start: u64) -> ([f64; 4], [f64; 4]) {
    use crate::modulation::resolve_parameter_at_time;

    let state = get_processing_state().await;
    let state_guard = state.read().await;
    let now = current_time_ms();

    let resolve_slots = |id: ChannelId| -> [f64; 4] {
        let src = &state_guard.channel(id).config.frequency;
        std::array::from_fn(|i| {
            let target = window_start + (i as u64) * 25;
            resolve_parameter_at_time(
                src,
                &state_guard.axis_values,
                &state_guard.no_input_behavior,
                now,
                state_guard.no_input_decay_ms,
                target,
            )
            .clamp(1.0, 200.0)
        })
    };

    (resolve_slots(ChannelId::A), resolve_slots(ChannelId::B))
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
        .axis_values
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
