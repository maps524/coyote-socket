/**
 * TypeScript types for Buttplug feature configuration
 * Defines feature types, configuration, and linking structure
 */

/**
 * Available Buttplug feature types that can be linked to parameters
 * Note: Position (ScalarCmd) is not used - clients prefer LinearCmd (PositionWithDuration)
 */
export type ButtplugFeatureType =
  | 'PositionWithDuration'
  | 'Vibrate'
  | 'Rotate'
  | 'Oscillate'
  | 'Constrict';

/**
 * Configuration for how many of each feature type to advertise
 * Default: 2 of each type (12 total features)
 */
export interface ButtplugFeatureConfig {
  position: number;              // Position feature count
  positionWithDuration: number;  // PositionWithDuration feature count
  vibrate: number;               // Vibrate feature count
  rotate: number;                // Rotate feature count
  oscillate: number;             // Oscillate feature count
  constrict: number;             // Constrict feature count
}

/**
 * Type-specific configuration for each Buttplug feature
 * Used in linking panel to control behavior of linked features
 */
export interface FeatureTypeConfig {
  // Vibrate
  distance?: number;        // Max amplitude of wobble (0.0-1.0)

  // Rotate
  scale?: number;           // How much of range to sweep (0.0-1.0)
  maxSpeed?: number;        // Max sweep rate in Hz (default: 5)

  // Oscillate
  // scale?: number;        // Shared with Rotate
  // maxSpeed?: number;     // Shared with Rotate

  // Constrict
  minFloor?: number;        // What "0" constriction means (0.0-1.0)
  useMidpoint?: boolean;    // Center around midpoint vs position
  method?: 'downsample' | 'clamp';  // How to apply bounds
}

/**
 * Link configuration for a single Buttplug feature to a parameter
 */
export interface ButtplugLinkConfig {
  featureType: ButtplugFeatureType;
  featureIndex: number;      // e.g., 0 for "Position 1", 1 for "Position 2"
  config?: FeatureTypeConfig;
}

/**
 * Default feature configuration
 * Note: position=0 because clients prefer LinearCmd (PositionWithDuration)
 */
export const defaultButtplugFeatureConfig: ButtplugFeatureConfig = {
  position: 0,
  positionWithDuration: 2,
  vibrate: 2,
  rotate: 2,
  oscillate: 2,
  constrict: 2
};

/**
 * Get total number of features from config
 */
export function getTotalFeatureCount(config: ButtplugFeatureConfig): number {
  return config.position +
         config.positionWithDuration +
         config.vibrate +
         config.rotate +
         config.oscillate +
         config.constrict;
}

/**
 * Get display name for a feature
 * @param type Feature type
 * @param index Feature index (0-based)
 * @returns Display name (e.g., "Position 1", "Vibrate 2")
 */
export function getFeatureDisplayName(type: ButtplugFeatureType, index: number): string {
  const baseName = type === 'PositionWithDuration' ? 'PosDur' : type;
  return `${baseName} ${index + 1}`;
}
