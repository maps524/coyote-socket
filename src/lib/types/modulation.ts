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

/**
 * Configuration for a single parameter's source. Sub G.3 unified the
 * Buttplug-feature pipeline into the same `transforms` vector that
 * T-Code / gamepad links use, dropping the legacy `buttplugLinks`
 * field and the parallel ecosystem branch.
 */
export interface ParameterSource {
  type: ParameterSourceType;

  // For 'static' mode
  staticValue?: number;

  // For 'linked' mode — any axis name the bus knows about
  // (`L0`, `R2`, `GP_LX`, `bp:Position_0`, ...).
  sourceAxis?: string;
  rangeMin: number;         // Output when input = 0%
  rangeMax: number;         // Output when input = 100%
  curve: CurveType;         // Transform curve
  curveStrength?: number;   // 0.1 - 3.0 for exp/log curves (default: 2.0)
  midpoint?: boolean;       // If true, input is distance from center (0.5 -> 0, 0 or 1 -> 1)
  delayMs?: number;         // Lag axis input by this many ms (0-200, step 25). 0/undefined = no delay.

  // Ordered shaping transforms attached to this link by the G.2 editor.
  // Round-trips as Vec<TransformConfig> directly into the runtime
  // ParameterLinkConfig.transforms. Optional + omitted-when-empty matches the
  // backend's `#[serde(default, skip_serializing_if = "Vec::is_empty")]`.
  transforms?: Transform[];
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
