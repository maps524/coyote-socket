// Parameter Modulation Types
// These types enable linking channel parameters to T-Code axes with curve transformations

/**
 * Source type for a parameter value
 * - static: Manual slider value
 * - linked: Dynamically controlled by T-Code axis
 */
export type ParameterSourceType = 'static' | 'linked';

/**
 * Curve transformation types for linked parameters
 * - linear: Direct 1:1 mapping (default)
 * - exponential: Slow start, fast end (good for intensity)
 * - logarithmic: Fast start, slow end (good for frequency)
 * - s-curve: Smooth ease in/out
 * - inverse: Flip the input (1 - value)
 */
export type CurveType = 'linear' | 'exponential' | 'logarithmic' | 's-curve' | 'inverse';

/**
 * Behavior when a linked axis has no incoming data
 * - hold: Keep the last received value (default)
 * - default: Fall back to static default value
 * - decay: Gradually decay to minimum over time
 * - zero: Immediately go to zero/minimum
 */
export type NoInputBehavior = 'hold' | 'default' | 'decay' | 'zero';

// ============================================================================
// Buttplug Feature Types (defined early for use in ParameterSource)
// ============================================================================

/**
 * Buttplug feature types that can be linked to channel parameters
 * Note: Position (ScalarCmd) is not used - clients prefer LinearCmd (PositionWithDuration)
 */
export type ButtplugFeatureType =
  | 'PositionWithDuration'
  | 'Vibrate'
  | 'Rotate'
  | 'Oscillate'
  | 'Constrict';

/**
 * Configuration specific to each Buttplug feature type
 */
export interface ButtplugFeatureConfig {
  // Vibrate
  distance?: number;              // 0.0-1.0, max amplitude of wobble (default: 0.2)

  // Rotate
  rotateScale?: number;           // 0.0-1.0, how much of range to sweep (default: 0.5)
  rotateMaxSpeed?: number;        // Hz, max sweep rate (default: 5.0)

  // Oscillate
  oscillateScale?: number;        // 0.0-1.0, portion of range to cover (default: 0.5)
  oscillateMaxSpeed?: number;     // Hz, max sweep rate (default: 5.0)

  // Constrict
  constrictMinFloor?: number;     // 0.0-1.0, what "0" constriction means (default: 0.0)
  constrictUseMidpoint?: boolean; // Center around midpoint vs position (default: false)
  constrictMethod?: 'downsample' | 'clamp'; // How to apply bounds (default: 'downsample')
}

/**
 * Buttplug feature link - identifies which feature is linked
 */
export interface ButtplugFeatureLink {
  featureType: ButtplugFeatureType;
  featureIndex: number;           // 0-based index (e.g., 0 for Position 1, 1 for Position 2)
  config?: ButtplugFeatureConfig;
}

/**
 * Buttplug links for a parameter (pipeline stages)
 */
export interface ButtplugLinks {
  position?: ButtplugFeatureLink;     // Position or PositionWithDuration (base value)
  motion?: ButtplugFeatureLink;       // Rotate or Oscillate (mutually exclusive)
  vibrate?: ButtplugFeatureLink;      // Vibrate (wobble modulation)
  constrict?: ButtplugFeatureLink;    // Constrict (range limiter)
}

/**
 * Configuration for a single parameter's source
 */
export interface ParameterSource {
  type: ParameterSourceType;

  // For 'static' mode
  staticValue?: number;

  // For 'linked' mode (T-Code)
  sourceAxis?: string;      // 'L0', 'L1', 'R0', 'R1', 'R2', 'V0-V3', 'A0-A1'
  rangeMin: number;         // Output when input = 0%
  rangeMax: number;         // Output when input = 100%
  curve: CurveType;         // Transform curve
  curveStrength?: number;   // 0.1 - 3.0 for exp/log curves (default: 2.0)
  midpoint?: boolean;       // If true, input is distance from center (0.5 -> 0, 0 or 1 -> 1)
  delayMs?: number;         // Lag axis input by this many ms (0-200, step 25). 0/undefined = no delay.

  // For Buttplug mode (pipeline stages)
  buttplugLinks?: ButtplugLinks;
}

/**
 * Complete configuration for a single channel's parameters
 */
export interface ChannelConfig {
  frequency: ParameterSource;         // 1-200 Hz
  frequencyBalance: ParameterSource;  // 0-255
  intensityBalance: ParameterSource;  // 0-255
  intensity: ParameterSource;         // 0-200 (range limited by min/max)
}

/**
 * General application settings for parameter modulation
 */
export interface GeneralSettings {
  noInputBehavior: NoInputBehavior;
  noInputDecayMs: number;       // Decay time for 'decay' behavior (100-2000ms)
  updateRateMs: number;         // Backend state update rate (10-100ms, default: 50ms)
  saveRateMs: number;           // File persistence rate (100-2000ms, default: 500ms)
  showTCodeMonitor: boolean;    // Toggle T-Code monitor visibility
}

/**
 * Default configuration for Channel A
 * Matches existing behavior: intensity linked to L0
 */
export const defaultChannelAConfig: ChannelConfig = {
  frequency: {
    type: 'static',
    staticValue: 100,
    rangeMin: 1,
    rangeMax: 200,
    curve: 'linear'
  },
  frequencyBalance: {
    type: 'static',
    staticValue: 128,
    rangeMin: 0,
    rangeMax: 255,
    curve: 'linear'
  },
  intensityBalance: {
    type: 'static',
    staticValue: 128,
    rangeMin: 0,
    rangeMax: 255,
    curve: 'linear'
  },
  intensity: {
    type: 'linked',
    sourceAxis: 'L0',
    rangeMin: 10,
    rangeMax: 20,
    curve: 'linear',
    curveStrength: 2.0
  }
};

/**
 * Default configuration for Channel B
 * Matches existing behavior: intensity linked to R2
 */
export const defaultChannelBConfig: ChannelConfig = {
  frequency: {
    type: 'static',
    staticValue: 100,
    rangeMin: 1,
    rangeMax: 200,
    curve: 'linear'
  },
  frequencyBalance: {
    type: 'static',
    staticValue: 128,
    rangeMin: 0,
    rangeMax: 255,
    curve: 'linear'
  },
  intensityBalance: {
    type: 'static',
    staticValue: 128,
    rangeMin: 0,
    rangeMax: 255,
    curve: 'linear'
  },
  intensity: {
    type: 'linked',
    sourceAxis: 'R2',
    rangeMin: 10,
    rangeMax: 20,
    curve: 'linear',
    curveStrength: 2.0
  }
};

/**
 * Default general settings
 */
export const defaultGeneralSettings: GeneralSettings = {
  noInputBehavior: 'hold',
  noInputDecayMs: 1000,
  updateRateMs: 50,
  saveRateMs: 500,
  showTCodeMonitor: false
};

// ============================================================================
// Curve Utility Functions
// ============================================================================

/**
 * Smoothstep function for S-curve (3t^2 - 2t^3)
 */
function smoothstep(t: number): number {
  const clamped = Math.max(0, Math.min(1, t));
  return clamped * clamped * (3 - 2 * clamped);
}

/**
 * Apply curve transformation to normalized input (0.0-1.0)
 * Matches the Rust implementation in modulation.rs
 */
export function applyCurve(input: number, curve: CurveType, strength: number = 2.0): number {
  const clamped = Math.max(0, Math.min(1, input));
  switch (curve) {
    case 'linear':
      return clamped;
    case 'exponential':
      return Math.pow(clamped, strength);
    case 'logarithmic':
      return Math.pow(clamped, 1.0 / strength);
    case 's-curve':
      return smoothstep(clamped);
    case 'inverse':
      return 1.0 - clamped;
    default:
      return clamped;
  }
}

/**
 * Linear interpolation between min and max
 */
export function lerp(min: number, max: number, t: number): number {
  const clamped = Math.max(0, Math.min(1, t));
  return min + (max - min) * clamped;
}

/**
 * Apply midpoint transformation if enabled
 * Converts input so center (0.5) becomes 0, and edges (0 or 1) become 1
 * Formula: abs(input - 0.5) * 2
 */
export function applyMidpoint(value: number): number {
  return Math.abs(value - 0.5) * 2;
}

/**
 * Apply curve and range mapping to a raw axis value
 * Returns a value in the range [0, 1] suitable for indicator display
 */
export function applySourceTransform(
  rawValue: number,
  source: ParameterSource
): number {
  if (source.type === 'static') {
    return 0; // No indicator for static sources
  }

  // Apply midpoint transformation first if enabled
  let value = source.midpoint ? applyMidpoint(rawValue) : rawValue;

  const strength = source.curveStrength ?? 2.0;
  return applyCurve(value, source.curve, strength);
}

// ============================================================================
// Buttplug Feature Defaults
// ============================================================================

/**
 * Default Buttplug feature configuration values
 */
export const defaultButtplugConfig: Required<ButtplugFeatureConfig> = {
  distance: 0.2,
  rotateScale: 0.5,
  rotateMaxSpeed: 5.0,
  oscillateScale: 0.5,
  oscillateMaxSpeed: 5.0,
  constrictMinFloor: 0.0,
  constrictUseMidpoint: false,
  constrictMethod: 'downsample'
};
