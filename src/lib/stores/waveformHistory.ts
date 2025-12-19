/**
 * Waveform History Store
 *
 * Tracks waveform samples for visualization.
 * Polls backend for new waveform data at 30Hz for smooth visualization.
 * Manages circular buffer on frontend for the configured time window.
 */

import { writable, derived } from 'svelte/store';
import { invoke } from '@tauri-apps/api/core';
import type { WaveformSample } from '$lib/types/waveform';

// ============================================================================
// Types
// ============================================================================

export interface WaveformHistory {
  samples: WaveformSample[];
  timeWindow: number;  // Seconds of history to keep
  lastFetchTimestamp: number;
}

interface WaveformStoreState {
  channelA: WaveformHistory;
  channelB: WaveformHistory;
  isPolling: boolean;
}

// ============================================================================
// Store State
// ============================================================================

const initialState: WaveformStoreState = {
  channelA: {
    samples: [],
    timeWindow: 2.0,  // Default 2 seconds
    lastFetchTimestamp: 0,
  },
  channelB: {
    samples: [],
    timeWindow: 2.0,
    lastFetchTimestamp: 0,
  },
  isPolling: false,
};

// Main writable store
const waveformStore = writable<WaveformStoreState>(initialState);

// ============================================================================
// Polling State
// ============================================================================

let pollIntervalId: ReturnType<typeof setInterval> | null = null;
let isPolling = false;

/**
 * Poll the backend for new waveform samples
 * Fetches samples since the last known timestamp
 */
async function pollWaveformData() {
  try {
    const state = getState();

    // Get the oldest timestamp we care about based on time window
    const now = Date.now();
    const windowMs = Math.max(state.channelA.timeWindow, state.channelB.timeWindow) * 1000;
    const cutoffTimestamp = now - windowMs;

    // Fetch samples since the last fetch (or since cutoff if first fetch)
    const sinceTimestamp = Math.max(
      state.channelA.lastFetchTimestamp,
      state.channelB.lastFetchTimestamp,
      cutoffTimestamp
    );

    // Call backend to get new samples
    const newSamples = await invoke<WaveformSample[]>('get_waveform_data', {
      sinceTimestamp: sinceTimestamp || 0,
    });

    if (newSamples.length === 0) {
      return; // No new data
    }

    // Update store with new samples
    waveformStore.update(state => {
      const nowTimestamp = Date.now();

      // Add new samples to channel A
      const channelASamples = [...state.channelA.samples, ...newSamples];
      const channelACutoff = nowTimestamp - (state.channelA.timeWindow * 1000);
      const prunedChannelA = channelASamples.filter(s => s.timestamp >= channelACutoff);

      // Add new samples to channel B (same samples, different channel data)
      const channelBSamples = [...state.channelB.samples, ...newSamples];
      const channelBCutoff = nowTimestamp - (state.channelB.timeWindow * 1000);
      const prunedChannelB = channelBSamples.filter(s => s.timestamp >= channelBCutoff);

      return {
        ...state,
        channelA: {
          ...state.channelA,
          samples: prunedChannelA,
          lastFetchTimestamp: nowTimestamp,
        },
        channelB: {
          ...state.channelB,
          samples: prunedChannelB,
          lastFetchTimestamp: nowTimestamp,
        },
      };
    });
  } catch (e) {
    // Silently ignore errors (backend might not be ready)
    console.debug('Waveform poll error:', e);
  }
}

/**
 * Get current store state (helper for reading inside update functions)
 */
function getState(): WaveformStoreState {
  let state: WaveformStoreState = initialState;
  waveformStore.subscribe(s => state = s)();
  return state;
}

// ============================================================================
// Public API
// ============================================================================

/**
 * Start polling for waveform data
 * Call this when visualization becomes active
 */
export function startWaveformTracking() {
  if (isPolling) return;

  isPolling = true;
  waveformStore.update(state => ({ ...state, isPolling: true }));

  // Poll at 30Hz (every ~33ms) for smooth visualization
  // This matches the inputPosition store polling rate
  pollIntervalId = setInterval(pollWaveformData, 33);

  // Do an immediate poll to get initial data
  pollWaveformData();
}

/**
 * Stop polling for waveform data
 * Call this when visualization is hidden/paused
 */
export function stopWaveformTracking() {
  isPolling = false;
  waveformStore.update(state => ({ ...state, isPolling: false }));

  if (pollIntervalId !== null) {
    clearInterval(pollIntervalId);
    pollIntervalId = null;
  }
}

/**
 * Clear all waveform history
 */
export function clearWaveformHistory() {
  waveformStore.set(initialState);
}

/**
 * Update time window for a channel
 * @param channel - 'A' or 'B'
 * @param seconds - Time window in seconds (1-10)
 */
export function setTimeWindow(channel: 'A' | 'B', seconds: number) {
  const clampedSeconds = Math.max(1, Math.min(10, seconds));

  waveformStore.update(state => {
    if (channel === 'A') {
      return {
        ...state,
        channelA: { ...state.channelA, timeWindow: clampedSeconds },
      };
    } else {
      return {
        ...state,
        channelB: { ...state.channelB, timeWindow: clampedSeconds },
      };
    }
  });
}

/**
 * Check if waveform tracking is active
 */
export function isTrackingActive(): boolean {
  return isPolling;
}

// ============================================================================
// Derived Stores
// ============================================================================

/**
 * Channel A waveform samples
 */
export const channelAWaveform = derived(
  waveformStore,
  $store => $store.channelA.samples
);

/**
 * Channel B waveform samples
 */
export const channelBWaveform = derived(
  waveformStore,
  $store => $store.channelB.samples
);

/**
 * Polling status
 */
export const waveformPolling = derived(
  waveformStore,
  $store => $store.isPolling
);

// Export the main store
export const waveformHistory = waveformStore;
