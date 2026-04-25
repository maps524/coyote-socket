use serde::{Deserialize, Serialize};

use crate::input_bus::InputBus;

/// State for smooth position interpolation (PositionWithDuration)
///
/// Tracks an ongoing movement from start_position to target_position over
/// duration_ms. New commands overwrite in-progress movements. `start_time`
/// is wall-clock milliseconds (matches `current_time_ms()` / `InputBus`
/// sample timestamps) so interpolation math reads as `now_ms - start_time`.
#[derive(Debug, Clone)]
pub struct PositionDurationState {
    /// When the movement started (wall-clock ms)
    pub start_time: u64,
    /// Position when movement began
    pub start_position: f64,
    /// Target position to reach
    pub target_position: f64,
    /// How long the movement should take (milliseconds)
    pub duration_ms: u32,
}

/// Per-channel processing state for Buttplug pipeline
///
/// Maintains all state needed to compute output from Buttplug feature inputs,
/// including position tracking, phase accumulators for oscillating features,
/// and interpolation state for smooth movements.
#[derive(Debug, Clone)]
pub struct ButtplugChannelState {
    /// Current base position (from Position or PositionWithDuration)
    /// Default: 0.5 (midpoint)
    pub base_position: f64,

    /// Active PositionWithDuration interpolation, if any
    pub pos_dur_state: Option<PositionDurationState>,

    /// Phase accumulator for Vibrate (radians)
    pub vibrate_phase: f64,

    /// Phase accumulator for Oscillate (0.0-inf, modulo 1.0 gives normalized phase)
    pub oscillate_phase: f64,

    /// Phase accumulator for Rotate (0.0-inf, modulo 1.0 gives normalized phase)
    pub rotate_phase: f64,

    /// Final output after pipeline processing (0.0-1.0)
    pub output: f64,
}

impl Default for ButtplugChannelState {
    fn default() -> Self {
        Self {
            base_position: 0.5,
            pos_dur_state: None,
            vibrate_phase: 0.0,
            oscillate_phase: 0.0,
            rotate_phase: 0.0,
            output: 0.5,
        }
    }
}

/// Current feature values from Buttplug client
///
/// Stores the latest values received for each feature. Features are indexed
/// within their type (e.g., Position 0, Position 1, Vibrate 0, Vibrate 1).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ButtplugFeatureValues {
    /// Position feature values (index → value 0.0-1.0)
    pub position: Vec<f64>,

    /// "New LinearCmd since last tick" buffer keyed by feature index. Each
    /// slot is `Some((position, duration_ms, arrival_ms))` when a new
    /// LinearCmd arrived after the channel's `last_buttplug_replay_ts`
    /// watermark, `None` otherwise. Skipped from serialization — the buffer
    /// is rebuilt fresh from bus state at every tick, never persisted.
    #[serde(skip)]
    pub position_with_duration: Vec<Option<(f64, u32, u64)>>,

    /// PositionWithDuration current values (index → position 0.0-1.0)
    /// Persists between ticks - used when no new LinearCmd is available
    pub position_with_duration_value: Vec<f64>,

    /// Vibrate feature values (index → speed 0.0-1.0)
    pub vibrate: Vec<f64>,

    /// Rotate feature values (index → (speed 0.0-1.0, clockwise bool))
    pub rotate: Vec<Option<(f64, bool)>>,

    /// Oscillate feature values (index → speed 0.0-1.0)
    pub oscillate: Vec<f64>,

    /// Constrict feature values (index → constriction 0.0-1.0)
    pub constrict: Vec<f64>,
}

impl ButtplugFeatureValues {
    /// Get Position value for a feature index
    pub fn get_position(&self, index: Option<usize>) -> Option<f64> {
        index.and_then(|i| self.position.get(i).copied())
    }

    /// Get new PositionWithDuration command for a feature index
    /// Returns (position, duration_ms, arrival_ms) if a new command is available
    pub fn get_new_position_with_duration(
        &self,
        index: Option<usize>,
    ) -> Option<(f64, u32, u64)> {
        index.and_then(|i| self.position_with_duration.get(i).and_then(|&cmd| cmd))
    }

    /// Get current PositionWithDuration value for a feature index
    /// This is the persisted position value, available even when no new LinearCmd
    pub fn get_position_with_duration_value(&self, index: Option<usize>) -> Option<f64> {
        index.and_then(|i| self.position_with_duration_value.get(i).copied())
    }

    /// Get Vibrate speed for a feature index
    pub fn get_vibrate(&self, index: Option<usize>) -> Option<f64> {
        index.and_then(|i| self.vibrate.get(i).copied())
    }

    /// Get Rotate parameters for a feature index
    /// Returns (speed, clockwise) if available
    pub fn get_rotate(&self, index: Option<usize>) -> Option<(f64, bool)> {
        index.and_then(|i| self.rotate.get(i).and_then(|&params| params))
    }

    /// Get Oscillate speed for a feature index
    pub fn get_oscillate(&self, index: Option<usize>) -> Option<f64> {
        index.and_then(|i| self.oscillate.get(i).copied())
    }

    /// Get Constrict value for a feature index
    pub fn get_constrict(&self, index: Option<usize>) -> Option<f64> {
        index.and_then(|i| self.constrict.get(i).copied())
    }

    /// Build a snapshot from the current `InputBus` state.
    ///
    /// Reads `bp:{FeatureType}_{i}` axes for `i in 0..max_features` and
    /// rebuilds the per-type vectors. The "new LinearCmd since last tick"
    /// buffer (`position_with_duration`) is populated from
    /// `bp:LinearCmd_{i}` axes whose latest sample timestamp is strictly
    /// greater than `last_replay_ts` — that's how the bundled phase replaces
    /// the old post-tick `clear()` of the linear-commands HashMap.
    pub fn from_input_bus(bus: &InputBus, last_replay_ts: u64, max_features: usize) -> Self {
        let mut result = Self {
            position: vec![0.0; max_features],
            position_with_duration: vec![None; max_features],
            // Default to midpoint so a channel without prior LinearCmd traffic
            // settles at center rather than zero.
            position_with_duration_value: vec![0.5; max_features],
            vibrate: vec![0.0; max_features],
            rotate: vec![None; max_features],
            oscillate: vec![0.0; max_features],
            constrict: vec![0.0; max_features],
        };

        for i in 0..max_features {
            if let Some(v) = bus.value(&format!("bp:Position_{}", i)) {
                result.position[i] = v;
            }
            if let Some(v) = bus.value(&format!("bp:Vibrate_{}", i)) {
                result.vibrate[i] = v;
            }
            if let Some(v) = bus.value(&format!("bp:Oscillate_{}", i)) {
                result.oscillate[i] = v;
            }
            if let Some(v) = bus.value(&format!("bp:Constrict_{}", i)) {
                result.constrict[i] = v;
            }
            if let Some(v) = bus.value(&format!("bp:PositionWithDuration_{}", i)) {
                result.position_with_duration_value[i] = v;
            }
            if let Some(speed) = bus.value(&format!("bp:Rotate_{}", i)) {
                let dir_axis = format!("bp:RotateDir_{}", i);
                let clockwise = bus.value(&dir_axis).map(|v| v >= 0.5).unwrap_or(true);
                result.rotate[i] = Some((speed, clockwise));
            }
            // LinearCmd "new arrival" detection: the axis stores
            // (position, duration_ms, arrival_ms) on each sample; we report
            // the latest only when its timestamp post-dates the watermark.
            if let Some(state) = bus.get(&format!("bp:LinearCmd_{}", i)) {
                if state.has_data && state.timestamp > last_replay_ts {
                    if let Some(sample) = state.history.back() {
                        result.position_with_duration[i] = Some((
                            sample.value,
                            sample.interval_ms.unwrap_or(0),
                            sample.timestamp,
                        ));
                    }
                }
            }
        }

        result
    }
}
