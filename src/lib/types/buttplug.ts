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
