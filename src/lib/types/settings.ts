/**
 * TypeScript types for backend settings
 * These match the Rust structs in src-tauri/src/settings.rs
 */

import type { ParameterSource, CurveType, ButtplugLinks } from './modulation.js';

export interface SavedBluetoothDevice {
    address: string;
    name: string | null;
}

/**
 * Buttplug feature link configuration for persistence
 * Uses string for featureType for JSON serialization compatibility
 */
export interface ButtplugFeatureLinkSettings {
    featureType: string;  // "Position", "Vibrate", etc.
    featureIndex: number;
    config: ButtplugFeatureConfigSettings;
}

/**
 * Configuration options for a Buttplug feature link
 */
export interface ButtplugFeatureConfigSettings {
    distance?: number;
    rotateScale?: number;
    rotateMaxSpeed?: number;
    oscillateScale?: number;
    oscillateMaxSpeed?: number;
    constrictMinFloor?: number;
    constrictUseMidpoint?: boolean;
    constrictMethod?: string;
}

/**
 * All Buttplug links for a single parameter (settings format)
 * Uses string-typed featureType for JSON serialization
 */
export interface ButtplugLinksSettings {
    position?: ButtplugFeatureLinkSettings;
    motion?: ButtplugFeatureLinkSettings;
    vibrate?: ButtplugFeatureLinkSettings;
    constrict?: ButtplugFeatureLinkSettings;
}

/**
 * Serializable version of ParameterSource for settings storage
 * All fields are always present to ensure proper serialization
 */
export interface ParameterSourceSettings {
    type: 'static' | 'linked';
    staticValue: number;      // Value when in static mode
    sourceAxis: string;       // Axis when in linked mode (e.g., 'L0', 'R2')
    rangeMin: number;         // Min output when linked
    rangeMax: number;         // Max output when linked
    curve: string;            // Curve type as string for serialization
    curveStrength: number;    // Curve strength (default 2.0)
    midpoint?: boolean;       // If true, use distance from center as input
    buttplugLinks?: ButtplugLinksSettings; // Buttplug feature links for this parameter
}

/**
 * Input ecosystem type for preset
 */
export type PresetEcosystem = 'tcode' | 'buttplug';

/**
 * A preset stores channel configuration for both channels
 */
export interface ChannelPreset {
    name: string;
    ecosystem: PresetEcosystem;
    channelA: ChannelSettings;
    channelB: ChannelSettings;
}

export interface ConnectionSettings {
    websocketPort: number;
    autoOpen: boolean;
    showTcodeMonitor: boolean;
}

export interface BluetoothSettings {
    selectedInterface: number;
    autoScan: boolean;
    autoConnect: boolean;
    savedDevices: SavedBluetoothDevice[];
    lastDevice: string | null;
}

export interface OutputSettings {
    processingEngine: string;
    peakFill?: string;
}

/**
 * Legacy channel settings format (for migration from old settings)
 */
export interface LegacyChannelSettings {
    frequency: number;
    freqBalance: number;
    intBalance: number;
    rangeMin: number;
    rangeMax: number;
}

/**
 * New channel settings with full parameter source support
 * Stores both static values and linked ranges for each parameter
 */
export interface ChannelSettings {
    frequencySource: ParameterSourceSettings;
    frequencyBalanceSource: ParameterSourceSettings;
    intensityBalanceSource: ParameterSourceSettings;
    intensitySource: ParameterSourceSettings;
}

export type AxisDir = 'pos' | 'neg';

export type ChordPart =
    | { kind: 'button'; index: number }
    | { kind: 'axis'; index: number; dir: AxisDir; threshold: number };

export type GamepadBinding =
    | { kind: 'button'; index: number }
    | { kind: 'axis'; index: number; dir: AxisDir; threshold: number }
    | { kind: 'combo'; parts: ChordPart[] };

/**
 * Action name → gamepad binding. Free-form map so new actions can be added
 * without schema changes. Mirrors src-tauri/src/settings.rs GamepadBindings.
 */
export type GamepadBindings = Record<string, GamepadBinding>;

export interface KeyboardShortcuts {
    channelAFreqUp: string;
    channelAFreqDown: string;
    channelAIntUp: string;
    channelAIntDown: string;
    channelAFreqBalUp: string;
    channelAFreqBalDown: string;
    channelAIntBalUp: string;
    channelAIntBalDown: string;
    channelBFreqUp: string;
    channelBFreqDown: string;
    channelBIntUp: string;
    channelBIntDown: string;
    channelBFreqBalUp: string;
    channelBFreqBalDown: string;
    channelBIntBalUp: string;
    channelBIntBalDown: string;
    help: string;
    settings: string;
    toggleOutputPause: string;
}

export interface GeneralSettings {
    noInputBehavior: string;
    noInputDecayMs: number;
    updateRateMs: number;
    saveRateMs: number;
    showTcodeMonitor: boolean;
    processingEngine: string;
    gamepadEngine?: 'off' | 'gilrs' | 'xinput';
    gamepadStickSensitivity?: number;
    gamepadButtonRepeatDelayMs?: number;
    gamepadButtonRepeatIntervalMs?: number;
    channelAMaxIntensity?: number;
    channelBMaxIntensity?: number;
}

export interface AppSettings {
    connection: ConnectionSettings;
    bluetooth: BluetoothSettings;
    output: OutputSettings;
    channelA: ChannelSettings;
    channelB: ChannelSettings;
    shortcuts: KeyboardShortcuts;
    general?: GeneralSettings;
    gamepadBindings?: GamepadBindings;
}

// Note: Default values are defined in Rust (src-tauri/src/settings.rs)
// The frontend fetches settings from the backend - no duplicate defaults needed here.

/**
 * Convert ParameterSource (store format) to ParameterSourceSettings (storage format)
 */
export function parameterSourceToSettings(source: ParameterSource): ParameterSourceSettings {
    return {
        type: source.type,
        staticValue: source.staticValue ?? 100,
        sourceAxis: source.sourceAxis ?? 'L0',
        rangeMin: source.rangeMin,
        rangeMax: source.rangeMax,
        curve: source.curve,
        curveStrength: source.curveStrength ?? 2.0,
        midpoint: source.midpoint
    };
}

/**
 * Convert ParameterSourceSettings (storage format) to ParameterSource (store format)
 */
export function settingsToParameterSource(settings: ParameterSourceSettings): ParameterSource {
    return {
        type: settings.type,
        staticValue: settings.staticValue,
        sourceAxis: settings.sourceAxis,
        rangeMin: settings.rangeMin,
        rangeMax: settings.rangeMax,
        curve: settings.curve as CurveType,
        curveStrength: settings.curveStrength,
        midpoint: settings.midpoint
    };
}

/**
 * Check if settings are in legacy format
 */
export function isLegacyChannelSettings(settings: unknown): settings is LegacyChannelSettings {
    if (!settings || typeof settings !== 'object') return false;
    const s = settings as Record<string, unknown>;
    return 'frequency' in s && typeof s.frequency === 'number' &&
           !('frequencySource' in s);
}

/**
 * Migrate legacy channel settings to new format
 */
export function migrateLegacyChannelSettings(
    legacy: LegacyChannelSettings,
    channel: 'A' | 'B'
): ChannelSettings {
    const defaultAxis = channel === 'A' ? 'L0' : 'R2';

    return {
        frequencySource: {
            type: 'static',
            staticValue: legacy.frequency,
            sourceAxis: defaultAxis,
            rangeMin: 1,
            rangeMax: 200,
            curve: 'linear',
            curveStrength: 2.0
        },
        frequencyBalanceSource: {
            type: 'static',
            staticValue: legacy.freqBalance,
            sourceAxis: defaultAxis,
            rangeMin: 0,
            rangeMax: 255,
            curve: 'linear',
            curveStrength: 2.0
        },
        intensityBalanceSource: {
            type: 'static',
            staticValue: legacy.intBalance,
            sourceAxis: defaultAxis,
            rangeMin: 0,
            rangeMax: 255,
            curve: 'linear',
            curveStrength: 2.0
        },
        intensitySource: {
            type: 'linked',
            staticValue: 100,
            sourceAxis: defaultAxis,
            rangeMin: legacy.rangeMin,
            rangeMax: legacy.rangeMax,
            curve: 'linear',
            curveStrength: 2.0
        }
    };
}

/**
 * Convert ButtplugLinksSettings (settings format with string featureType)
 * to ButtplugLinks (runtime format with union type featureType)
 */
export function settingsToButtplugLinks(settings: ButtplugLinksSettings | undefined): ButtplugLinks | undefined {
    if (!settings) return undefined;

    const convertLink = (link: ButtplugFeatureLinkSettings | undefined) => {
        if (!link) return undefined;
        return {
            featureType: link.featureType as any, // Trust the string matches the union
            featureIndex: link.featureIndex,
            config: link.config ? {
                distance: link.config.distance,
                rotateScale: link.config.rotateScale,
                rotateMaxSpeed: link.config.rotateMaxSpeed,
                oscillateScale: link.config.oscillateScale,
                oscillateMaxSpeed: link.config.oscillateMaxSpeed,
                constrictMinFloor: link.config.constrictMinFloor,
                constrictUseMidpoint: link.config.constrictUseMidpoint,
                constrictMethod: link.config.constrictMethod as any
            } : undefined
        };
    };

    return {
        position: convertLink(settings.position),
        motion: convertLink(settings.motion),
        vibrate: convertLink(settings.vibrate),
        constrict: convertLink(settings.constrict)
    };
}

/**
 * Convert ButtplugLinks (runtime format) to ButtplugLinksSettings (settings format)
 */
export function buttplugLinksToSettings(links: ButtplugLinks | undefined): ButtplugLinksSettings | undefined {
    if (!links) return undefined;

    const convertLink = (link: any): ButtplugFeatureLinkSettings | undefined => {
        if (!link) return undefined;
        return {
            featureType: link.featureType,
            featureIndex: link.featureIndex,
            config: {
                distance: link.config?.distance,
                rotateScale: link.config?.rotateScale,
                rotateMaxSpeed: link.config?.rotateMaxSpeed,
                oscillateScale: link.config?.oscillateScale,
                oscillateMaxSpeed: link.config?.oscillateMaxSpeed,
                constrictMinFloor: link.config?.constrictMinFloor,
                constrictUseMidpoint: link.config?.constrictUseMidpoint,
                constrictMethod: link.config?.constrictMethod
            }
        };
    };

    return {
        position: convertLink(links.position),
        motion: convertLink(links.motion),
        vibrate: convertLink(links.vibrate),
        constrict: convertLink(links.constrict)
    };
}
