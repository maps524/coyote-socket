use serde::{Deserialize, Serialize};

/// Buttplug feature output types
///
/// Each type serves a specific role in the processing pipeline:
/// - Position: Base value
/// - PositionWithDuration: Smooth movement
/// - Vibrate: High-frequency wobble
/// - Rotate: Directional sweep (sawtooth)
/// - Oscillate: Alternating sweep (triangle)
/// - Constrict: Range limiter
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ButtplugFeatureType {
    /// Sets exact target position (base value)
    Position,
    /// Moves from current to target position over specified duration
    PositionWithDuration,
    /// Adds oscillating wobble around center point
    Vibrate,
    /// Repeating motion in one direction (sawtooth wave)
    Rotate,
    /// Repeating back-and-forth motion (triangle wave)
    Oscillate,
    /// Bounds the output range (downsamples or clamps)
    Constrict,
}

/// Method for applying Constrict bounds
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConstrictionMethod {
    /// Remap 0.0-1.0 input to constrained range (preserves relative position)
    Downsample,
    /// Cut off values outside bounds (can cause flat spots)
    Clamp,
}

impl Default for ConstrictionMethod {
    fn default() -> Self {
        ConstrictionMethod::Downsample
    }
}

/// Configuration for how many features of each type to advertise
///
/// This determines how many features of each type the virtual Buttplug device
/// will report as available.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ButtplugFeatureConfig {
    /// Number of Position features (default: 2)
    pub position: usize,
    /// Number of PositionWithDuration features (default: 2)
    pub position_with_duration: usize,
    /// Number of Vibrate features (default: 2)
    pub vibrate: usize,
    /// Number of Rotate features (default: 2)
    pub rotate: usize,
    /// Number of Oscillate features (default: 2)
    pub oscillate: usize,
    /// Number of Constrict features (default: 2)
    pub constrict: usize,
}

impl Default for ButtplugFeatureConfig {
    fn default() -> Self {
        Self {
            position: 0,  // Not used - clients prefer LinearCmd (PositionWithDuration)
            position_with_duration: 2,
            vibrate: 2,
            rotate: 2,
            oscillate: 2,
            constrict: 2,
        }
    }
}

impl ButtplugFeatureConfig {
    /// Get total number of features configured
    pub fn total_features(&self) -> usize {
        self.position
            + self.position_with_duration
            + self.vibrate
            + self.rotate
            + self.oscillate
            + self.constrict
    }
}

/// Type-specific configuration for a feature
///
/// Different feature types use different config parameters:
/// - Vibrate: distance
/// - Rotate: scale, max_speed
/// - Oscillate: scale, max_speed
/// - Constrict: min_floor, use_midpoint, method
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureTypeConfig {
    // Vibrate parameters
    /// Max amplitude of wobble (0.0-1.0), default: 0.2
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance: Option<f64>,

    // Rotate/Oscillate parameters
    /// Portion of range to sweep (0.0-1.0), default: 0.5
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    /// Max sweep rate in Hz, default: 5.0
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_speed: Option<f64>,

    // Constrict parameters
    /// What "0" constriction means (0.0-1.0), default: 0.0
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_floor: Option<f64>,
    /// Center around midpoint vs position, default: false
    #[serde(skip_serializing_if = "Option::is_none")]
    pub use_midpoint: Option<bool>,
    /// How to apply bounds, default: Downsample
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<ConstrictionMethod>,
}

impl Default for FeatureTypeConfig {
    fn default() -> Self {
        Self {
            distance: Some(0.2),
            scale: Some(0.5),
            max_speed: Some(5.0),
            min_floor: Some(0.0),
            use_midpoint: Some(false),
            method: Some(ConstrictionMethod::Downsample),
        }
    }
}

/// Configuration for which features are linked to which channel parameter
///
/// Defines the connections from Buttplug features to a single channel parameter
/// (e.g., Channel A Intensity). Each feature type can have at most one feature
/// linked per parameter.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ButtplugLinkConfig {
    /// Which Position feature is linked (feature index), if any
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position_feature: Option<usize>,

    /// Which PositionWithDuration feature is linked (feature index), if any
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pos_dur_feature: Option<usize>,

    /// Which Vibrate feature is linked (feature index), if any
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vibrate_feature: Option<usize>,
    /// Configuration for the linked Vibrate feature
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vibrate_config: Option<FeatureTypeConfig>,

    /// Which Rotate feature is linked (feature index), if any
    /// Note: Mutually exclusive with oscillate_feature
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotate_feature: Option<usize>,
    /// Configuration for the linked Rotate feature
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotate_config: Option<FeatureTypeConfig>,

    /// Which Oscillate feature is linked (feature index), if any
    /// Note: Mutually exclusive with rotate_feature
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oscillate_feature: Option<usize>,
    /// Configuration for the linked Oscillate feature
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oscillate_config: Option<FeatureTypeConfig>,

    /// Which Constrict feature is linked (feature index), if any
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constrict_feature: Option<usize>,
    /// Configuration for the linked Constrict feature
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constrict_config: Option<FeatureTypeConfig>,
}

impl ButtplugLinkConfig {
    /// Check if any features are linked in this configuration
    pub fn has_any_links(&self) -> bool {
        self.position_feature.is_some()
            || self.pos_dur_feature.is_some()
            || self.vibrate_feature.is_some()
            || self.rotate_feature.is_some()
            || self.oscillate_feature.is_some()
            || self.constrict_feature.is_some()
    }
}
