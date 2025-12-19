use serde::{Deserialize, Serialize};
use std::time::Instant;

/// State for smooth position interpolation (PositionWithDuration)
///
/// Tracks an ongoing movement from start_position to target_position over duration_ms.
/// New commands overwrite in-progress movements.
#[derive(Debug, Clone)]
pub struct PositionDurationState {
    /// When the movement started
    pub start_time: Instant,
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

    /// PositionWithDuration commands (index → (position, duration_ms, arrival_time))
    /// This is a "new command" buffer that gets cleared after processing
    /// Skipped from serialization because Instant can't be serialized
    #[serde(skip)]
    pub position_with_duration: Vec<Option<(f64, u32, std::time::Instant)>>,

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
    /// Returns (position, duration_ms, arrival_time) if a new command is available
    pub fn get_new_position_with_duration(&self, index: Option<usize>) -> Option<(f64, u32, std::time::Instant)> {
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

    /// Clear new command buffers (called after processing each tick)
    pub fn clear_new_commands(&mut self) {
        self.position_with_duration.iter_mut().for_each(|cmd| *cmd = None);
    }

    /// Create ButtplugFeatureValues from a HashMap of feature keys
    ///
    /// Keys are expected to be in the format "{FeatureType}_{Index}", e.g.:
    /// - "Vibrate_0", "Vibrate_1"
    /// - "Position_0"
    /// - "Oscillate_0"
    /// - "Constrict_0"
    /// - "Rotate_0"
    ///
    /// The linear_commands map should contain PositionWithDuration (LinearCmd) values
    /// as (position, duration_ms, arrival_time) tuples indexed by feature index.
    pub fn from_hashmap(
        features: &std::collections::HashMap<String, f64>,
        linear_commands: &std::collections::HashMap<usize, (f64, u32, std::time::Instant)>,
        rotate_directions: &std::collections::HashMap<usize, bool>,
        max_features: usize,
    ) -> Self {
        let mut result = Self {
            position: vec![0.0; max_features],
            position_with_duration: vec![None; max_features],
            position_with_duration_value: vec![0.5; max_features], // Default to midpoint
            vibrate: vec![0.0; max_features],
            rotate: vec![None; max_features],
            oscillate: vec![0.0; max_features],
            constrict: vec![0.0; max_features],
        };

        // Parse feature keys and populate vectors
        for (key, value) in features {
            if let Some((feature_type, index)) = parse_feature_key(key) {
                if index < max_features {
                    match feature_type.as_str() {
                        "Position" => result.position[index] = *value,
                        "PositionWithDuration" => result.position_with_duration_value[index] = *value,
                        "Vibrate" => result.vibrate[index] = *value,
                        "Oscillate" => result.oscillate[index] = *value,
                        "Constrict" => result.constrict[index] = *value,
                        "Rotate" => {
                            let clockwise = rotate_directions.get(&index).copied().unwrap_or(true);
                            result.rotate[index] = Some((*value, clockwise));
                        }
                        _ => {}
                    }
                }
            }
        }

        // Populate LinearCmd (PositionWithDuration) new command values with arrival time
        for (index, (position, duration, arrival_time)) in linear_commands {
            if *index < max_features {
                result.position_with_duration[*index] = Some((*position, *duration, *arrival_time));
            }
        }

        result
    }
}

/// Parse a feature key like "Vibrate_0" into (feature_type, index)
fn parse_feature_key(key: &str) -> Option<(String, usize)> {
    let parts: Vec<&str> = key.split('_').collect();
    if parts.len() == 2 {
        if let Ok(index) = parts[1].parse::<usize>() {
            return Some((parts[0].to_string(), index));
        }
    }
    None
}
