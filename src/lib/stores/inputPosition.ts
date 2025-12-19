/**
 * Input Position Store
 *
 * Tracks the current T-Code input position for each channel and all axes.
 * Receives updates via Tauri events (synchronized with 10Hz device loop).
 * Uses requestAnimationFrame for smooth UI interpolation.
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

// Smoothing factor (0-1, higher = faster response)
// At 10Hz input rate, 0.4 gives smooth but responsive feel
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

  // Update raw stores (unsmoothed values)
  rawPosition.set({
    channelA: payload.channel_a,
    channelB: payload.channel_b,
    timestamp: payload.timestamp
  });

  axisValues.set(payload.axes);
}

/**
 * Start listening for axis updates and animation
 * Call this when the app starts or when input monitoring is needed
 */
export async function startInputTracking() {
  if (isTracking) return;

  isTracking = true;

  // Listen for axis-update events from backend (emitted at 10Hz)
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
