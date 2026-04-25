/**
 * Input Position Store
 *
 * Tracks the current T-Code input position for each channel and all axes.
 * Receives updates via `axis-update` Tauri events, which fire per inbound
 * T-Code/Buttplug message — cadence depends on the sender and can range from
 * ~10Hz up to 60+ Hz. Uses requestAnimationFrame to lerp display values
 * toward the latest target so bar motion stays smooth at any input rate.
 */

import { writable, derived } from 'svelte/store';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

export interface InputPosition {
  channelA: number;  // 0.0 - 1.0 normalized
  channelB: number;  // 0.0 - 1.0 normalized
  timestamp: number;
}

export interface AxisValues {
  [axis: string]: number;  // 0.0 - 1.0 normalized (e.g., "L0": 0.5, "L1": 0.7, "R2": 0.3)
}

// Payload from backend event
interface AxisUpdatePayload {
  axes: Record<string, number>;
  channel_a: number;
  channel_b: number;
  timestamp: number;
}

// Raw position data from backend
const rawPosition = writable<InputPosition>({
  channelA: 0,
  channelB: 0,
  timestamp: 0
});

// Smoothed position for display (interpolated via RAF)
const smoothPosition = writable<InputPosition>({
  channelA: 0,
  channelB: 0,
  timestamp: 0
});

// All axis values (raw from backend)
const axisValues = writable<AxisValues>({});

// Smoothed axis values (interpolated via RAF)
const smoothAxisValues = writable<AxisValues>({});

// Animation state
let animationFrameId: number | null = null;
let isTracking = false;
let unlistenFn: UnlistenFn | null = null;

// Target values for interpolation (updated by events)
let targetA = 0;
let targetB = 0;
let currentA = 0;
let currentB = 0;

// Target and current values for all axes
let targetAxes: AxisValues = {};
let currentAxes: AxisValues = {};

// Per-axis raw sample history with timestamps (for delayed indicator lookup).
// Trimmed at write time to AXIS_HISTORY_MS so memory stays bounded. Sized to
// cover the maximum supported input delay (1000ms) with headroom.
const AXIS_HISTORY_MS = 1500;
const axisHistory: Map<string, Array<{ ts: number; value: number }>> = new Map();

// Smoothing factor (0-1, higher = faster response). Applied each RAF tick
// (~60Hz), so 0.4 roughly halves distance-to-target every 2 frames — fast
// enough to stay responsive when inputs arrive at 20-60Hz, slow enough to
// hide step-jitter when the sender drops to ~10Hz.
const SMOOTHING = 0.4;

/**
 * Animation loop using requestAnimationFrame
 * Smoothly interpolates current values toward target values
 */
function animate() {
  // Lerp toward target for channels
  currentA += (targetA - currentA) * SMOOTHING;
  currentB += (targetB - currentB) * SMOOTHING;

  // Lerp toward target for all axes
  const newSmoothAxes: AxisValues = {};
  for (const axis in targetAxes) {
    const target = targetAxes[axis] ?? 0;
    const current = currentAxes[axis] ?? 0;
    const smoothed = current + (target - current) * SMOOTHING;
    currentAxes[axis] = smoothed;
    newSmoothAxes[axis] = smoothed;
  }

  // Update the smooth stores
  smoothPosition.set({
    channelA: currentA,
    channelB: currentB,
    timestamp: Date.now()
  });

  smoothAxisValues.set(newSmoothAxes);

  // Continue animation if tracking is active
  if (isTracking) {
    animationFrameId = requestAnimationFrame(animate);
  }
}

/**
 * Handle incoming axis update event from backend
 */
function handleAxisUpdate(payload: AxisUpdatePayload) {
  // Update target values for interpolation
  targetA = payload.channel_a;
  targetB = payload.channel_b;
  targetAxes = payload.axes;

  // Append raw samples to per-axis history for delayed-indicator lookup.
  const cutoff = payload.timestamp - AXIS_HISTORY_MS;
  for (const axis in payload.axes) {
    let ring = axisHistory.get(axis);
    if (!ring) {
      ring = [];
      axisHistory.set(axis, ring);
    }
    ring.push({ ts: payload.timestamp, value: payload.axes[axis] });
    while (ring.length > 0 && ring[0].ts < cutoff) {
      ring.shift();
    }
  }

  // Update raw stores (unsmoothed values)
  rawPosition.set({
    channelA: payload.channel_a,
    channelB: payload.channel_b,
    timestamp: payload.timestamp
  });

  axisValues.set(payload.axes);
}

/**
 * Look up an axis value at `msAgo` milliseconds in the past, linearly
 * interpolating between bracketing samples so the indicator slides smoothly
 * between input events instead of stair-stepping at the input cadence.
 *
 * - Target between two samples → linear interp by timestamp.
 * - Target newer than the latest sample → return latest (held).
 * - Target older than the oldest retained sample → return oldest.
 * - No history at all → 0.
 *
 * Used by the indicator when a parameter has `delayMs` set, so the visual
 * position reflects what the device is *actually* outputting (which lags
 * the live input by `delayMs`).
 */
export function axisValueAt(axis: string, msAgo: number): number {
  const ring = axisHistory.get(axis);
  if (!ring || ring.length === 0) return 0;
  if (msAgo <= 0) return ring[ring.length - 1].value;
  const target = Date.now() - msAgo;

  // Newer than newest sample → hold latest.
  const last = ring[ring.length - 1];
  if (target >= last.ts) return last.value;
  // Older than oldest retained sample → return oldest.
  if (target <= ring[0].ts) return ring[0].value;

  // Walk newest → oldest to find the first sample at-or-before `target`.
  // Its successor (one index higher) is the first sample strictly after.
  for (let i = ring.length - 1; i > 0; i--) {
    const after = ring[i];
    const before = ring[i - 1];
    if (before.ts <= target && target <= after.ts) {
      const span = after.ts - before.ts;
      if (span <= 0) return after.value;
      const t = (target - before.ts) / span;
      return before.value + (after.value - before.value) * t;
    }
  }
  return last.value;
}

/**
 * Start listening for axis updates and animation
 * Call this when the app starts or when input monitoring is needed
 */
export async function startInputTracking() {
  if (isTracking) return;

  isTracking = true;

  // Listen for axis-update events (fired per inbound input message, variable rate)
  try {
    unlistenFn = await listen<AxisUpdatePayload>('axis-update', (event) => {
      handleAxisUpdate(event.payload);
    });
  } catch (e) {
    console.error('Failed to listen for axis updates:', e);
  }

  // Start animation loop for smooth interpolation
  animationFrameId = requestAnimationFrame(animate);
}

/**
 * Stop listening and animation
 * Call this when input monitoring is not needed (e.g., app hidden)
 */
export function stopInputTracking() {
  isTracking = false;

  if (unlistenFn !== null) {
    unlistenFn();
    unlistenFn = null;
  }

  if (animationFrameId !== null) {
    cancelAnimationFrame(animationFrameId);
    animationFrameId = null;
  }
}

/**
 * Check if tracking is active
 */
export function isTrackingActive(): boolean {
  return isTracking;
}

// Derived stores for individual channels (convenience)
export const inputPositionA = derived(smoothPosition, $pos => $pos.channelA);
export const inputPositionB = derived(smoothPosition, $pos => $pos.channelB);

// Export the main stores
export const inputPosition = smoothPosition;
export const rawInputPosition = rawPosition;

// Export axis stores
export const allAxisValues = smoothAxisValues;
export const rawAxisValues = axisValues;

/**
 * Get the current smoothed value for a specific axis
 * Returns 0 if the axis doesn't exist
 */
export function getAxisValue(axisValues: AxisValues, axis: string): number {
  return axisValues[axis] ?? 0;
}
