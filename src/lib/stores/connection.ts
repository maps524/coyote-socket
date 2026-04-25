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
    label: 'v1 Queued',
    description: 'Original queue-based ramping. Each tick advances toward the latest target at a fixed rate, so big jumps take several ticks to reach. Predictable but lags fast input.',
  },
  {
    value: 'v2-smooth',
    label: 'v2 Smooth',
    description: 'Averages incoming samples within each output bucket. Smooths out noisy or rapidly-changing input — best for ambient or sustained sensations.',
  },
  {
    value: 'v2-balanced',
    label: 'v2 Balanced',
    description: 'Linear interpolation between the most recent samples. Good general-purpose default; smooth transitions without losing too much detail.',
  },
  {
    value: 'v2-detailed',
    label: 'v2 Detailed',
    description: 'Peak-preserving downsample. Empty buckets inherit a neighbor (configurable via Peak Fill) so brief spikes from impacts or rhythm are kept rather than averaged away.',
  },
  {
    value: 'v2-dynamic',
    label: 'v2 Dynamic',
    description: 'Alternates min and max values across consecutive output slots so rapid oscillations (fast back-and-forth input) survive downsampling instead of cancelling.',
  },
  {
    value: 'v2-sustained',
    label: 'v2 Sustained',
    description: 'Dynamic shape with a 200 ms peak-hold on master intensity. Brief peaks linger so felt strength stays during fast pole-flicks instead of dropping between strikes.',
  },
  {
    value: 'v3-predictive',
    label: 'v3 Predictive',
    description: 'Buffers ~1 s of commands and generates smooth ramps between known points using lookahead. Highest fidelity for scripted input; adds ~1 s latency.',
  },
] as const;

/** Processing engine type derived from config */
export type ProcessingEngine = (typeof PROCESSING_ENGINES)[number]['value'];

/**
 * Peak-fill strategy — orthogonal variant selector for the V2 Detailed engine.
 * Shown in the UI beside the Engine picker. Only affects v2-detailed; other
 * engines ignore it.
 */
export const PEAK_FILL_STRATEGIES = [
  {
    value: 'forward',
    label: 'Forward Fill',
    description: 'Empty buckets inherit the next sample. Stronger peak preservation.',
  },
  {
    value: 'legacy',
    label: 'Cascade',
    description: 'Empty buckets inherit the previous bucket. Original behavior.',
  },
] as const;

export type PeakFillStrategy = (typeof PEAK_FILL_STRATEGIES)[number]['value'];

export interface OutputOptions {
  processingEngine: ProcessingEngine;
  peakFill: PeakFillStrategy;
}

export const outputOptions = writable<OutputOptions>({
  processingEngine: 'v2-smooth',
  peakFill: 'forward',
});

export interface ConnectionStatus {
  connected: boolean;
}

export const connectionStatus = writable<ConnectionStatus>({
  connected: false,
});