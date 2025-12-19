import { writable } from 'svelte/store';

export interface ConnectionState {
  websocketConnected: boolean;
  bluetoothConnected: boolean;
  bluetoothDevice?: string;
  batteryLevel?: number;
}

export const connectionState = writable<ConnectionState>({
  websocketConnected: false,
  bluetoothConnected: false,
});

/**
 * Processing engine configuration - single source of truth
 * Add new engines here and they'll appear in both the type and UI
 */
export const PROCESSING_ENGINES = [
  {
    value: 'v1',
    label: 'v1 (Queue-based)',
    description: 'Original queue-based ramping implementation',
  },
  {
    value: 'v2-smooth',
    label: 'v2 Smooth (Averaging)',
    description: 'Averaging downsampling - best for ambient/sustained sensations',
  },
  {
    value: 'v2-balanced',
    label: 'v2 Balanced (Recommended)',
    description: 'Linear interpolation - general use, smooth transitions',
  },
  {
    value: 'v2-detailed',
    label: 'v2 Detailed (Peak-preserving)',
    description: 'Preserves intensity spikes - best for impacts/rhythm',
  },
  {
    value: 'v2-dynamic',
    label: 'v2 Dynamic (Oscillation)',
    description: 'Preserves rapid oscillations by alternating min/max values',
  },
  {
    value: 'v3-predictive',
    label: 'v3 Predictive (Lookahead)',
    description: 'Buffers 1s of commands to generate smooth ramps between points',
  },
] as const;

/** Processing engine type derived from config */
export type ProcessingEngine = (typeof PROCESSING_ENGINES)[number]['value'];

export interface OutputOptions {
  processingEngine: ProcessingEngine;
}

export const outputOptions = writable<OutputOptions>({
  processingEngine: 'v2-smooth',
});

export interface ConnectionStatus {
  connected: boolean;
}

export const connectionStatus = writable<ConnectionStatus>({
  connected: false,
});