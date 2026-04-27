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

  // Ordered shaping transforms attached to this link by the new G.2 editor.
  // Round-trips as Vec<TransformConfig> directly into the runtime
  // ParameterLinkConfig.transforms. Optional + omitted-when-empty matches the
  // backend's `#[serde(default, skip_serializing_if = "Vec::is_empty")]`.
  transforms?: Transform[];

  // For Buttplug mode (pipeline stages) — legacy; G.3 retires this in favor
  // of the unified `transforms` list. The convert layer prefers `transforms`
  // when non-empty; when empty, it falls back to translating
  // `buttplugLinks` so saved Buttplug presets keep working.
  buttplugLinks?: ButtplugLinks;
}

// ============================================================================
// Transforms (sub G.2)
// ============================================================================

/**
 * Per-tag discriminated union mirroring the Rust `TransformConfig` enum in
 * `src-tauri/src/transforms/mod.rs`. The backend uses
 * `#[serde(tag = "type", rename_all = "kebab-case")]` plus per-field
 * `#[serde(rename = "...")]` for the camelCase modifier-axis names. Field
 * names without a `rename` keep their snake_case Rust names (e.g.
 * `time_constant_ms`, `duration_ms`).
 *
 * Variants split into two categories:
 *
 * - **Generic primitives** (`smooth`, `scale`, `clamp`, `invert`, `hold`,
 *   `mix`) — composable shaping functions. Read at most one bus modifier
 *   axis (Mix); the others are pure value-in / value-out.
 * - **Buttplug semantic wrappers** (`vibrate`, `oscillate`, `rotate`,
 *   `constrict`) — single-purpose variants that carry the labels users
 *   recognize from the legacy Buttplug pipeline. `rotate` is the only
 *   variant that declares two modifier axes (speed + direction).
 */
export type Transform =
  | { type: 'smooth'; time_constant_ms: number }
  | { type: 'scale'; factor: number }
  | { type: 'clamp'; min: number; max: number }
  | { type: 'invert' }
  | { type: 'hold'; duration_ms: number }
  | { type: 'mix'; otherAxis: string; weight: number }
  | { type: 'vibrate'; speedAxis: string; distance: number }
  | { type: 'oscillate'; speedAxis: string; scale: number; maxSpeedHz: number }
  | { type: 'rotate'; speedAxis: string; directionAxis: string; scale: number; maxSpeedHz: number }
  | {
      type: 'constrict';
      amountAxis: string;
      minFloor: number;
      useMidpoint: boolean;
      method: 'Downsample' | 'Clamp';
    };

/** All transform variant tags, in editor display order. */
export const TRANSFORM_TYPES: ReadonlyArray<Transform['type']> = [
  'smooth',
  'scale',
  'clamp',
  'invert',
  'hold',
  'mix',
  'vibrate',
  'oscillate',
  'rotate',
  'constrict'
] as const;

/** Human-readable labels for the variant picker dropdown. */
export const TRANSFORM_LABELS: Record<Transform['type'], string> = {
  smooth: 'Smooth',
  scale: 'Scale',
  clamp: 'Clamp',
  invert: 'Invert',
  hold: 'Hold',
  mix: 'Mix',
  vibrate: 'Vibrate',
  oscillate: 'Oscillate',
  rotate: 'Rotate',
  constrict: 'Constrict'
};

/**
 * Defaults for newly added transforms. Mirrors the backend's
 * `TransformConfig::initial_state` priming: zero-state types start at zero,
 * Buttplug wrappers default to the legacy `convert_parameter_source` defaults
 * (Vibrate distance 0.2, Oscillate/Rotate scale 0.5 + 5Hz, Constrict
 * downsample). Modifier-axis fields default to empty so the editor surfaces
 * them as "fill me in".
 */
export function defaultTransform(type: Transform['type']): Transform {
  switch (type) {
    case 'smooth':
      return { type: 'smooth', time_constant_ms: 100 };
    case 'scale':
      return { type: 'scale', factor: 1 };
    case 'clamp':
      return { type: 'clamp', min: 0, max: 1 };
    case 'invert':
      return { type: 'invert' };
    case 'hold':
      return { type: 'hold', duration_ms: 200 };
    case 'mix':
      return { type: 'mix', otherAxis: '', weight: 0.5 };
    case 'vibrate':
      return { type: 'vibrate', speedAxis: '', distance: 0.2 };
    case 'oscillate':
      return { type: 'oscillate', speedAxis: '', scale: 0.5, maxSpeedHz: 5 };
    case 'rotate':
      return {
        type: 'rotate',
        speedAxis: '',
        directionAxis: '',
        scale: 0.5,
        maxSpeedHz: 5
      };
    case 'constrict':
      return {
        type: 'constrict',
        amountAxis: '',
        minFloor: 0,
        useMidpoint: false,
        method: 'Downsample'
      };
  }
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
// Math utilities
// ============================================================================

/**
 * Linear interpolation between min and max.
 */
export function lerp(min: number, max: number, t: number): number {
  const clamped = Math.max(0, Math.min(1, t));
  return min + (max - min) * clamped;
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
