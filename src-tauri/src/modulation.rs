// Parameter Modulation Module
// Handles dynamic parameter linking to T-Code axes with curve transformations

use crate::settings::ButtplugLinksSettings;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Source type for a parameter value
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ParameterSourceType {
    Static,
    Linked,
}

/// Curve transformation types for linked parameters
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum CurveType {
    Linear,
    Exponential,
    Logarithmic,
    SCurve,
    Inverse,
}

/// Behavior when a linked axis has no incoming data
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum NoInputBehavior {
    Hold,
    Default,
    Decay,
    Zero,
}

/// Configuration for a single parameter's source
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParameterSource {
    #[serde(rename = "type")]
    pub source_type: ParameterSourceType,

    // For 'static' mode
    #[serde(skip_serializing_if = "Option::is_none")]
    pub static_value: Option<f64>,

    // For 'linked' mode
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_axis: Option<String>,
    pub range_min: f64,
    pub range_max: f64,
    pub curve: CurveType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub curve_strength: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub midpoint: Option<bool>, // If true, input is distance from center (0.5 → 0, 0 or 1 → 1)

    // For Buttplug mode (pipeline stages)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub buttplug_links: Option<ButtplugLinksSettings>,
}

impl ParameterSource {
    /// Create a static parameter source
    pub fn static_source(value: f64) -> Self {
        Self {
            source_type: ParameterSourceType::Static,
            static_value: Some(value),
            source_axis: None,
            range_min: 0.0,
            range_max: 0.0,
            curve: CurveType::Linear,
            curve_strength: None,
            midpoint: None,
            buttplug_links: None,
        }
    }

    /// Create a linked parameter source
    pub fn linked_source(axis: &str, min: f64, max: f64, curve: CurveType) -> Self {
        Self {
            source_type: ParameterSourceType::Linked,
            static_value: None,
            source_axis: Some(axis.to_string()),
            range_min: min,
            range_max: max,
            curve,
            curve_strength: Some(2.0),
            midpoint: None,
            buttplug_links: None,
        }
    }
}

/// Complete configuration for a single channel's parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelConfig {
    pub frequency: ParameterSource,
    pub frequency_balance: ParameterSource,
    pub intensity_balance: ParameterSource,
    pub intensity: ParameterSource,
}

impl Default for ChannelConfig {
    fn default() -> Self {
        Self {
            frequency: ParameterSource::static_source(100.0),
            frequency_balance: ParameterSource::static_source(128.0),
            intensity_balance: ParameterSource::static_source(128.0),
            intensity: ParameterSource::linked_source("L0", 10.0, 20.0, CurveType::Linear),
        }
    }
}

impl ChannelConfig {
    /// Create default configuration for Channel A (linked to L0)
    pub fn channel_a_default() -> Self {
        Self {
            frequency: ParameterSource::static_source(100.0),
            frequency_balance: ParameterSource::static_source(128.0),
            intensity_balance: ParameterSource::static_source(128.0),
            intensity: ParameterSource::linked_source("L0", 10.0, 20.0, CurveType::Linear),
        }
    }

    /// Create default configuration for Channel B (linked to R2)
    pub fn channel_b_default() -> Self {
        Self {
            frequency: ParameterSource::static_source(100.0),
            frequency_balance: ParameterSource::static_source(128.0),
            intensity_balance: ParameterSource::static_source(128.0),
            intensity: ParameterSource::linked_source("R2", 10.0, 20.0, CurveType::Linear),
        }
    }
}

/// Retention window for per-axis sample history. Slightly wider than the
/// 100ms output window so per-slot lookups at `ws+0..+75` can always find a
/// sample at or before the target time, even when input is sparse.
pub const AXIS_HISTORY_MS: u64 = 500;

/// State tracking for a single T-Code axis. `value`/`timestamp` hold the most
/// recent sample (cheap hot-path access); `history` preserves timestamped
/// samples for per-slot lookups (e.g. sub-100ms frequency modulation).
#[derive(Debug, Clone)]
pub struct AxisState {
    pub value: f64,     // 0.0-1.0 normalized (most recent)
    pub timestamp: u64, // When last updated (milliseconds)
    pub has_data: bool, // Has received any data this session
    /// Ordered-by-time ring of recent samples. Trimmed to AXIS_HISTORY_MS.
    pub history: std::collections::VecDeque<(u64, f64)>,
}

impl Default for AxisState {
    fn default() -> Self {
        Self {
            value: 0.0,
            timestamp: 0,
            has_data: false,
            history: std::collections::VecDeque::with_capacity(64),
        }
    }
}

impl AxisState {
    /// Create a new axis state with a value
    #[cfg(test)]
    pub fn new(value: f64, timestamp: u64) -> Self {
        let v = value.clamp(0.0, 1.0);
        let mut history = std::collections::VecDeque::with_capacity(64);
        history.push_back((timestamp, v));
        Self {
            value: v,
            timestamp,
            has_data: true,
            history,
        }
    }

    /// Update the axis value and append to history, trimming old samples.
    pub fn update(&mut self, value: f64, timestamp: u64) {
        let v = value.clamp(0.0, 1.0);
        self.value = v;
        self.timestamp = timestamp;
        self.has_data = true;
        self.history.push_back((timestamp, v));
        let cutoff = timestamp.saturating_sub(AXIS_HISTORY_MS);
        while let Some(&(ts, _)) = self.history.front() {
            if ts < cutoff {
                self.history.pop_front();
            } else {
                break;
            }
        }
    }

    /// Look up the axis value at a historical timestamp using nearest-prior
    /// sample (zero-order hold). Returns the current `value` if the target
    /// is newer than all samples, or `None` if history is empty.
    pub fn value_at(&self, target_time: u64) -> Option<f64> {
        if self.history.is_empty() {
            return if self.has_data { Some(self.value) } else { None };
        }
        // Scan from newest back; first sample with ts <= target wins.
        let mut best: Option<f64> = None;
        for &(ts, v) in self.history.iter().rev() {
            if ts <= target_time {
                best = Some(v);
                break;
            }
        }
        // target predates all samples → clamp to oldest available.
        best.or_else(|| self.history.front().map(|&(_, v)| v))
    }
}

/// Apply curve transformation to normalized input (0.0-1.0)
pub fn apply_curve(input: f64, curve: &CurveType, strength: f64) -> f64 {
    let input = input.clamp(0.0, 1.0);
    match curve {
        CurveType::Linear => input,
        CurveType::Exponential => input.powf(strength),
        CurveType::Logarithmic => input.powf(1.0 / strength),
        CurveType::SCurve => smoothstep(input),
        CurveType::Inverse => 1.0 - input,
    }
}

/// Apply midpoint transformation
/// Converts input so center (0.5) becomes 0, and edges (0 or 1) become 1
/// Formula: abs(input - 0.5) * 2
pub fn apply_midpoint(input: f64) -> f64 {
    (input - 0.5).abs() * 2.0
}

/// Smoothstep function for S-curve (3t^2 - 2t^3)
fn smoothstep(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Linear interpolation
pub fn lerp(min: f64, max: f64, t: f64) -> f64 {
    min + (max - min) * t.clamp(0.0, 1.0)
}

/// Resolve a parameter source to its value at a specific point in time.
///
/// Like `resolve_parameter` but indexes the axis history at `target_time_ms`
/// instead of using the latest sample. Lets callers compute per-slot values
/// within a 100ms output window (e.g. for V3 B0's 4-slot frequency array).
pub fn resolve_parameter_at_time(
    source: &ParameterSource,
    axis_values: &HashMap<String, AxisState>,
    no_input_behavior: &NoInputBehavior,
    current_time_ms: u64,
    no_input_decay_ms: u32,
    target_time_ms: u64,
) -> f64 {
    match source.source_type {
        ParameterSourceType::Static => source.static_value.unwrap_or(0.0),
        ParameterSourceType::Linked => {
            let axis = source.source_axis.as_ref();
            let axis_state = axis.and_then(|a| axis_values.get(a));

            let input = match axis_state {
                Some(state) if state.has_data => {
                    // Staleness check is against CURRENT time (not target), so a
                    // stale axis produces the same no-input response for every
                    // slot within one tick.
                    let age_ms = current_time_ms.saturating_sub(state.timestamp);
                    if age_ms > 1000 {
                        handle_no_input(
                            no_input_behavior,
                            source,
                            state,
                            age_ms,
                            no_input_decay_ms,
                        )
                    } else {
                        state
                            .value_at(target_time_ms)
                            .unwrap_or(state.value)
                    }
                }
                _ => handle_no_input_no_state(no_input_behavior, source),
            };

            let midpoint_value = if source.midpoint.unwrap_or(false) {
                apply_midpoint(input)
            } else {
                input
            };

            let strength = source.curve_strength.unwrap_or(2.0);
            let curved = apply_curve(midpoint_value, &source.curve, strength);
            lerp(source.range_min, source.range_max, curved)
        }
    }
}

/// Resolve a parameter source to its current value
pub fn resolve_parameter(
    source: &ParameterSource,
    axis_values: &HashMap<String, AxisState>,
    no_input_behavior: &NoInputBehavior,
    current_time_ms: u64,
    no_input_decay_ms: u32,
) -> f64 {
    match source.source_type {
        ParameterSourceType::Static => source.static_value.unwrap_or(0.0),
        ParameterSourceType::Linked => {
            let axis = source.source_axis.as_ref();
            let axis_state = axis.and_then(|a| axis_values.get(a));

            let input = match axis_state {
                Some(state) if state.has_data => {
                    // Check if we should apply no-input behavior based on staleness
                    let age_ms = current_time_ms.saturating_sub(state.timestamp);
                    if age_ms > 1000 {
                        // No data for over 1 second
                        handle_no_input(no_input_behavior, source, state, age_ms, no_input_decay_ms)
                    } else {
                        state.value
                    }
                }
                _ => {
                    // No data available
                    handle_no_input_no_state(no_input_behavior, source)
                }
            };

            // Apply midpoint transformation if enabled (before curve)
            let midpoint_value = if source.midpoint.unwrap_or(false) {
                apply_midpoint(input)
            } else {
                input
            };

            let strength = source.curve_strength.unwrap_or(2.0);
            let curved = apply_curve(midpoint_value, &source.curve, strength);
            lerp(source.range_min, source.range_max, curved)
        }
    }
}

/// Handle no-input behavior when axis state exists but is stale
fn handle_no_input(
    behavior: &NoInputBehavior,
    source: &ParameterSource,
    state: &AxisState,
    age_ms: u64,
    decay_ms: u32,
) -> f64 {
    match behavior {
        NoInputBehavior::Hold => state.value,
        NoInputBehavior::Default => source.static_value.unwrap_or(0.0),
        NoInputBehavior::Zero => 0.0,
        NoInputBehavior::Decay => {
            // Decay from last value to zero over decay_ms
            let decay_progress = (age_ms as f64 / decay_ms as f64).min(1.0);
            state.value * (1.0 - decay_progress)
        }
    }
}

/// Handle no-input behavior when no axis state exists
fn handle_no_input_no_state(behavior: &NoInputBehavior, source: &ParameterSource) -> f64 {
    match behavior {
        NoInputBehavior::Hold => 0.0,
        NoInputBehavior::Default => source.static_value.unwrap_or(0.0),
        NoInputBehavior::Zero => 0.0,
        NoInputBehavior::Decay => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apply_curve_linear() {
        assert_eq!(apply_curve(0.5, &CurveType::Linear, 2.0), 0.5);
    }

    #[test]
    fn test_apply_curve_exponential() {
        let result = apply_curve(0.5, &CurveType::Exponential, 2.0);
        assert!((result - 0.25).abs() < 0.001);
    }

    #[test]
    fn test_apply_curve_inverse() {
        assert_eq!(apply_curve(0.3, &CurveType::Inverse, 2.0), 0.7);
    }

    #[test]
    fn test_smoothstep() {
        assert_eq!(smoothstep(0.0), 0.0);
        assert_eq!(smoothstep(1.0), 1.0);
        assert!((smoothstep(0.5) - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_lerp() {
        assert_eq!(lerp(0.0, 100.0, 0.5), 50.0);
        assert_eq!(lerp(10.0, 20.0, 1.0), 20.0);
    }

    #[test]
    fn test_axis_state_update() {
        let mut state = AxisState::default();
        assert!(!state.has_data);

        state.update(0.75, 1000);
        assert!(state.has_data);
        assert_eq!(state.value, 0.75);
        assert_eq!(state.timestamp, 1000);
    }

    #[test]
    fn test_resolve_static_parameter() {
        let source = ParameterSource::static_source(42.0);
        let axis_values = HashMap::new();
        let result = resolve_parameter(&source, &axis_values, &NoInputBehavior::Hold, 0, 1000);
        assert_eq!(result, 42.0);
    }

    #[test]
    fn test_resolve_linked_parameter() {
        let source = ParameterSource::linked_source("L0", 0.0, 100.0, CurveType::Linear);
        let mut axis_values = HashMap::new();
        axis_values.insert("L0".to_string(), AxisState::new(0.5, 100));

        let result = resolve_parameter(&source, &axis_values, &NoInputBehavior::Hold, 200, 1000);
        assert_eq!(result, 50.0);
    }
}
