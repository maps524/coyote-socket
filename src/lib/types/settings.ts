/**
 * TypeScript types for backend settings
 * These match the Rust structs in src-tauri/src/settings.rs
 */

import type { ParameterSource, CurveType, Transform } from './modulation.js';

export interface SavedBluetoothDevice {
    address: string;
    name: string | null;
}

/**
 * Serializable version of ParameterSource for settings storage. Sub G.3
 * dropped the legacy `buttplugLinks` field; the unified `transforms`
 * vector replaces it.
 */
export interface ParameterSourceSettings {
    type: 'static' | 'linked';
    staticValue: number;      // Value when in static mode
    sourceAxis: string;       // Axis when in linked mode (e.g., 'L0', 'R2', 'bp:Vibrate_0')
    rangeMin: number;         // Min output when linked
    rangeMax: number;         // Max output when linked
    curve: string;            // Curve type as string for serialization
    curveStrength: number;    // Curve strength (default 2.0)
    midpoint?: boolean;       // If true, use distance from center as input
    delayEnabled?: boolean;   // Whether input delay is active (separate from value)
    delayMs?: number;         // Input delay in ms (0-200, step 25); only honored if delayEnabled
    transforms?: Transform[]; // Ordered shaping transforms attached by the editor (sub G.2+)
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
        midpoint: source.midpoint,
        delayEnabled: source.delayMs !== undefined,
        delayMs: source.delayMs ?? 0
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
        midpoint: settings.midpoint,
        delayMs: settings.delayEnabled ? (settings.delayMs ?? 0) : undefined
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

