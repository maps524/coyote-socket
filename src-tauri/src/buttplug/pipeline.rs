use super::state::{ButtplugChannelState, ButtplugFeatureValues};
use super::types::{ButtplugLinkConfig, ConstrictionMethod};
use std::f64::consts::PI;
use std::time::Instant;

/// Process the Buttplug feature pipeline for a single channel parameter
///
/// Pipeline order:
/// 1. Position/PositionWithDuration → Base value
/// 2. Motion (Rotate OR Oscillate) → Adds movement pattern
/// 3. Vibrate → Adds high-frequency wobble
/// 4. Constrict → Downsamples to final range
///
/// # Arguments
/// * `state` - Mutable channel state (position, phases, interpolation)
/// * `features` - Current feature values from Buttplug client
/// * `config` - Link configuration (which features affect this parameter)
/// * `now` - Current timestamp for interpolation
/// * `dt_ms` - Delta time since last tick (milliseconds)
///
/// # Returns
/// Final output value clamped to 0.0-1.0
pub fn process_buttplug_pipeline(
    state: &mut ButtplugChannelState,
    features: &ButtplugFeatureValues,
    config: &ButtplugLinkConfig,
    now: Instant,
    dt_ms: u32,
) -> f64 {
    let mut value: f64;

    // ========== STAGE 1: POSITION - Set base value ==========

    // Position: Set base value directly
    if let Some(pos) = features.get_position(config.position_feature) {
        state.base_position = pos;
    }

    // PositionWithDuration: Check for new LinearCmd with duration
    let new_pos_dur = features.get_new_position_with_duration(config.pos_dur_feature);
    if let Some((target, duration, arrival_time)) = new_pos_dur {
        println!("[Pipeline] New PositionWithDuration: target={:.3}, duration={}ms, start_pos={:.3}, arrived={:?}ms ago",
            target, duration, state.base_position, now.duration_since(arrival_time).as_millis());
        // New command received - start interpolation using arrival time as start
        // This ensures smooth interpolation even when commands arrive faster than tick rate
        state.pos_dur_state = Some(super::state::PositionDurationState {
            start_time: arrival_time,
            start_position: state.base_position,
            target_position: target,
            duration_ms: duration,
        });
    }

    // Process ongoing interpolation
    if let Some(ref pds) = state.pos_dur_state {
        let elapsed_ms = now.duration_since(pds.start_time).as_millis() as f64;
        let progress = (elapsed_ms / pds.duration_ms as f64).min(1.0);
        state.base_position = lerp(pds.start_position, pds.target_position, progress);

        // Clear interpolation state when complete
        if progress >= 1.0 {
            state.pos_dur_state = None;
        }
    } else if new_pos_dur.is_none() {
        // No new LinearCmd and no active interpolation - use persisted position value
        // This handles the case where linear_commands was cleared but we have the latest position
        if let Some(pos) = features.get_position_with_duration_value(config.pos_dur_feature) {
            state.base_position = pos;
        }
    }

    // DEBUG: Show position state
    let pos_dur_val = features.get_position_with_duration_value(config.pos_dur_feature);
    let interp_progress = state.pos_dur_state.as_ref().map(|pds| {
        let elapsed = now.duration_since(pds.start_time).as_millis() as f64;
        (elapsed / pds.duration_ms as f64).min(1.0) * 100.0
    });
    println!("[Pipeline] pos_dur_feature={:?}, new_cmd={}, interp_progress={:?}%, pos_dur_val={:?}, base_pos={:.3}",
        config.pos_dur_feature, new_pos_dur.is_some(), interp_progress, pos_dur_val, state.base_position);

    value = state.base_position;

    // ========== STAGE 2: MOTION - Oscillate OR Rotate (mutually exclusive) ==========

    // Oscillate: Triangle wave centered on base
    if let Some(speed) = features.get_oscillate(config.oscillate_feature) {
        let config_vals = config.oscillate_config.as_ref();
        let scale = config_vals.and_then(|c| c.scale).unwrap_or(0.5);
        let max_speed = config_vals.and_then(|c| c.max_speed).unwrap_or(5.0);

        let freq_hz = speed * max_speed; // 0-5 Hz oscillation rate
        state.oscillate_phase += freq_hz * (dt_ms as f64 / 1000.0);

        let phase_norm = state.oscillate_phase % 1.0;
        // Triangle wave: 0→1→0 over one cycle
        let triangle = 1.0 - (2.0 * phase_norm - 1.0).abs();
        // Center at 0.5, then scale and multiply by 2 to get ±scale range
        let offset = (triangle - 0.5) * 2.0 * scale;
        value += offset;
    }
    // Rotate: Sawtooth wave (directional sweep)
    else if let Some((speed, clockwise)) = features.get_rotate(config.rotate_feature) {
        let config_vals = config.rotate_config.as_ref();
        let scale = config_vals.and_then(|c| c.scale).unwrap_or(0.5);
        let max_speed = config_vals.and_then(|c| c.max_speed).unwrap_or(5.0);

        let freq_hz = speed * max_speed; // 0-5 Hz sweep rate
        state.rotate_phase += freq_hz * (dt_ms as f64 / 1000.0);

        let sawtooth = state.rotate_phase % 1.0;
        let direction = if clockwise { 1.0 } else { -1.0 };
        let offset = sawtooth * scale * direction;
        value += offset;
    }

    // ========== STAGE 3: VIBRATE - Add wobble ==========

    if let Some(speed) = features.get_vibrate(config.vibrate_feature) {
        let config_vals = config.vibrate_config.as_ref();
        let distance = config_vals.and_then(|c| c.distance).unwrap_or(0.2);

        let freq_hz = speed * 20.0; // 0-20 Hz vibration
        state.vibrate_phase += freq_hz * (dt_ms as f64 / 1000.0) * 2.0 * PI;

        let offset = state.vibrate_phase.sin() * distance;
        value += offset;
    }

    // ========== STAGE 4: CONSTRICT - Downsample to range ==========

    // Debug: Check constrict configuration and values
    println!("[Pipeline] Constrict check: constrict_feature={:?}, constrict_values={:?}",
        config.constrict_feature, features.constrict);

    if let Some(constriction) = features.get_constrict(config.constrict_feature) {
        let config_vals = config.constrict_config.as_ref();
        let min_floor = config_vals.and_then(|c| c.min_floor).unwrap_or(0.0);
        let use_midpoint = config_vals.and_then(|c| c.use_midpoint).unwrap_or(false);
        let method = config_vals
            .and_then(|c| c.method)
            .unwrap_or(ConstrictionMethod::Downsample);

        // Calculate effective range: at constriction=0, range is full; at 1.0, range is min_floor
        // Higher constriction = more constrained = smaller range
        let effective = lerp(1.0, min_floor, constriction);

        // Determine center point for constriction
        let center = if use_midpoint {
            0.5
        } else {
            state.base_position
        };

        // Calculate bounds
        let half_range = effective * 0.5;
        let min_bound = (center - half_range).max(0.0);
        let max_bound = (center + half_range).min(1.0);

        let pre_constrict = value;
        value = match method {
            ConstrictionMethod::Downsample => {
                // Remap 0.0-1.0 input to constrained range
                let normalized = value.clamp(0.0, 1.0);
                min_bound + (normalized * (max_bound - min_bound))
            }
            ConstrictionMethod::Clamp => {
                // Cut off at bounds
                value.clamp(min_bound, max_bound)
            }
        };

        println!("[Pipeline] Constrict: constriction={:.3}, effective_range={:.3}, bounds=[{:.3}, {:.3}], pre={:.3}, post={:.3}",
            constriction, effective, min_bound, max_bound, pre_constrict, value);
    }

    // ========== FINAL OUTPUT ==========

    state.output = value.clamp(0.0, 1.0);
    state.output
}

/// Linear interpolation helper
#[inline]
fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buttplug::types::FeatureTypeConfig;

    #[test]
    fn test_pipeline_position_only() {
        let mut state = ButtplugChannelState::default();
        let mut features = ButtplugFeatureValues::default();
        features.position = vec![0.7];

        let config = ButtplugLinkConfig {
            position_feature: Some(0),
            ..Default::default()
        };

        let now = Instant::now();
        let output = process_buttplug_pipeline(&mut state, &features, &config, now, 100);

        assert_eq!(output, 0.7);
        assert_eq!(state.base_position, 0.7);
    }

    #[test]
    fn test_pipeline_vibrate_centers_on_position() {
        let mut state = ButtplugChannelState::default();
        let mut features = ButtplugFeatureValues::default();
        features.position = vec![0.5];
        features.vibrate = vec![0.5]; // 50% speed → 10Hz

        let config = ButtplugLinkConfig {
            position_feature: Some(0),
            vibrate_feature: Some(0),
            vibrate_config: Some(FeatureTypeConfig {
                distance: Some(0.2),
                ..Default::default()
            }),
            ..Default::default()
        };

        let now = Instant::now();
        let output = process_buttplug_pipeline(&mut state, &features, &config, now, 100);

        // Output should be 0.5 ± 0.2 depending on phase
        assert!(output >= 0.3 && output <= 0.7);
    }

    #[test]
    fn test_pipeline_constrict_downsample() {
        let mut state = ButtplugChannelState {
            base_position: 0.5,
            ..Default::default()
        };
        let mut features = ButtplugFeatureValues::default();
        features.constrict = vec![0.5]; // 50% constriction

        let config = ButtplugLinkConfig {
            constrict_feature: Some(0),
            constrict_config: Some(FeatureTypeConfig {
                min_floor: Some(0.0),
                use_midpoint: Some(true),
                method: Some(ConstrictionMethod::Downsample),
                ..Default::default()
            }),
            ..Default::default()
        };

        let now = Instant::now();

        // Start at base_position = 0.5
        state.base_position = 0.5;
        let output = process_buttplug_pipeline(&mut state, &features, &config, now, 100);

        // With constriction=0.5, min_floor=0.0:
        // effective = lerp(1.0, 0.0, 0.5) = 0.5 (50% of full range)
        // Centered at 0.5 (use_midpoint=true), half_range=0.25
        // Bounds are [0.25, 0.75]
        // Input value 0.5 (center) maps to center of constrained range = 0.5
        assert!((output - 0.5).abs() < 0.01);
    }

    #[test]
    fn test_pipeline_oscillate() {
        let mut state = ButtplugChannelState {
            base_position: 0.5,
            ..Default::default()
        };
        let mut features = ButtplugFeatureValues::default();
        features.oscillate = vec![1.0]; // Max speed

        let config = ButtplugLinkConfig {
            oscillate_feature: Some(0),
            oscillate_config: Some(FeatureTypeConfig {
                scale: Some(0.4),
                max_speed: Some(5.0),
                ..Default::default()
            }),
            ..Default::default()
        };

        let now = Instant::now();
        let output1 = process_buttplug_pipeline(&mut state, &features, &config, now, 100);

        // Run again to advance phase
        let output2 = process_buttplug_pipeline(&mut state, &features, &config, now, 100);

        // Outputs should differ as phase advances
        assert_ne!(output1, output2);

        // Both should be within base ± scale range
        assert!(output1 >= 0.1 && output1 <= 0.9);
        assert!(output2 >= 0.1 && output2 <= 0.9);
    }

    #[test]
    fn test_lerp() {
        assert_eq!(lerp(0.0, 1.0, 0.0), 0.0);
        assert_eq!(lerp(0.0, 1.0, 0.5), 0.5);
        assert_eq!(lerp(0.0, 1.0, 1.0), 1.0);
        assert_eq!(lerp(0.2, 0.8, 0.5), 0.5);
    }
}
