/**
 * Resolved State Store
 *
 * Subscribes to the backend `resolved-update` Tauri event (10Hz device tick)
 * and exposes per-channel post-resolver snapshots for UI position indicators.
 * Replaces `inputPosition.ts` — the previous store mirrored raw axis values
 * and applied curves frontend-side; this one reads what the resolver actually
 * computed (curve + transforms + delay applied), so position indicators show
 * where the device is actually being driven, not just where the controller is.
 *
 * Sub G.1: only `intensity` is used by the existing UI; the other three
 * parameter slots (frequency / frequency_balance / intensity_balance) are
 * captured here for the curve plot dot work in G.2.
 */
import { writable, derived, type Readable } from 'svelte/store';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

/**
 * One parameter slot's resolver output projected onto the wire format.
 *
 * Snake_case JSON keys preserved verbatim — matches `WaveformSample` and
 * `AxisUpdatePayload` conventions; the backend struct in `resolver.rs` has
 * no `#[serde(rename_all)]` attribute so the TS shape mirrors Rust field
 * names directly.
 *
 * `source_axis` is omitted from the wire format for Static parameters
 * (`#[serde(skip_serializing_if = "Option::is_none")]`); UI code infers
 * Static from `source_axis === undefined` rather than reading a separate
 * boolean. The G.0 backend currently also emits a redundant `is_static`
 * field; sub G.1 follow-up drops it.
 */
export interface ResolvedSampleSnapshot {
  raw_input: number;             // pre-curve, pre-transforms axis read at target_time
  normalized_pre_range: number;  // 0..1 post-curve+transforms; what UI plots
  device_value: number;          // device units (0-200 intensity, 1-200 Hz, ...)
  target_time_ms: number;        // now - delay_ms; lets UI align dots with delay
  source_axis?: string;          // absent => Static parameter
}

export interface ChannelResolvedSnapshot {
  frequency: ResolvedSampleSnapshot;
  frequency_balance: ResolvedSampleSnapshot;
  intensity_balance: ResolvedSampleSnapshot;
  intensity: ResolvedSampleSnapshot;
}

export interface ResolvedUpdatePayload {
  timestamp_ms: number;
  channel_a: ChannelResolvedSnapshot;
  channel_b: ChannelResolvedSnapshot;
}

const ZERO_SAMPLE: ResolvedSampleSnapshot = {
  raw_input: 0,
  normalized_pre_range: 0,
  device_value: 0,
  target_time_ms: 0
};

const ZERO_CHANNEL: ChannelResolvedSnapshot = {
  frequency: { ...ZERO_SAMPLE },
  frequency_balance: { ...ZERO_SAMPLE },
  intensity_balance: { ...ZERO_SAMPLE },
  intensity: { ...ZERO_SAMPLE }
};

// Latest target snapshot from backend (raw 10Hz arrivals).
const targetA = writable<ChannelResolvedSnapshot>({ ...ZERO_CHANNEL });
const targetB = writable<ChannelResolvedSnapshot>({ ...ZERO_CHANNEL });

// Smoothed snapshots updated each RAF tick.
const smoothA = writable<ChannelResolvedSnapshot>({ ...ZERO_CHANNEL });
const smoothB = writable<ChannelResolvedSnapshot>({ ...ZERO_CHANNEL });

// Mutable target/current state. The writable stores above mirror these for
// reactive consumers, but the lerp loop reads/writes these locals to avoid
// per-RAF subscribe→update churn through Svelte's reactivity machinery.
let latestA: ChannelResolvedSnapshot = { ...ZERO_CHANNEL };
let latestB: ChannelResolvedSnapshot = { ...ZERO_CHANNEL };
let currentA: ChannelResolvedSnapshot = { ...ZERO_CHANNEL };
let currentB: ChannelResolvedSnapshot = { ...ZERO_CHANNEL };

let isTracking = false;
let unlistenFn: UnlistenFn | null = null;
let animationFrameId: number | null = null;

// Same coefficient as inputPosition's RAF smoothing — distance-to-target
// roughly halves every 2 frames at ~60Hz, fast enough to keep up with 10Hz
// arrivals while damping the step edges.
const SMOOTHING = 0.4;

function lerpSample(curr: ResolvedSampleSnapshot, target: ResolvedSampleSnapshot): ResolvedSampleSnapshot {
  return {
    // Discrete fields pass through to the latest arrival; lerping a string
    // axis name or a target_time_ms across frames would only blur them.
    target_time_ms: target.target_time_ms,
    source_axis: target.source_axis,
    raw_input: curr.raw_input + (target.raw_input - curr.raw_input) * SMOOTHING,
    normalized_pre_range:
      curr.normalized_pre_range + (target.normalized_pre_range - curr.normalized_pre_range) * SMOOTHING,
    device_value: curr.device_value + (target.device_value - curr.device_value) * SMOOTHING
  };
}

function lerpChannel(
  curr: ChannelResolvedSnapshot,
  target: ChannelResolvedSnapshot
): ChannelResolvedSnapshot {
  return {
    frequency: lerpSample(curr.frequency, target.frequency),
    frequency_balance: lerpSample(curr.frequency_balance, target.frequency_balance),
    intensity_balance: lerpSample(curr.intensity_balance, target.intensity_balance),
    intensity: lerpSample(curr.intensity, target.intensity)
  };
}

function animate() {
  currentA = lerpChannel(currentA, latestA);
  currentB = lerpChannel(currentB, latestB);
  smoothA.set(currentA);
  smoothB.set(currentB);

  if (isTracking) {
    animationFrameId = requestAnimationFrame(animate);
  }
}

function handleResolvedUpdate(payload: ResolvedUpdatePayload) {
  latestA = payload.channel_a;
  latestB = payload.channel_b;
  targetA.set(latestA);
  targetB.set(latestB);
}

/**
 * Begin listening for `resolved-update` events and start RAF smoothing.
 * Idempotent.
 */
export async function startResolvedTracking(): Promise<void> {
  if (isTracking) return;
  isTracking = true;

  try {
    unlistenFn = await listen<ResolvedUpdatePayload>('resolved-update', (event) => {
      handleResolvedUpdate(event.payload);
    });
  } catch (e) {
    console.error('Failed to listen for resolved updates:', e);
  }

  animationFrameId = requestAnimationFrame(animate);
}

/**
 * Stop the listener and the RAF loop. Safe to call when not running.
 */
export function stopResolvedTracking(): void {
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

export function isResolvedTrackingActive(): boolean {
  return isTracking;
}

/** Smoothed per-channel snapshot. Lerped each RAF toward the latest 10Hz arrival. */
export const resolvedChannelA: Readable<ChannelResolvedSnapshot> = { subscribe: smoothA.subscribe };
export const resolvedChannelB: Readable<ChannelResolvedSnapshot> = { subscribe: smoothB.subscribe };

/** Raw (unlerped) per-channel snapshot — direct event payload. Useful for
 *  consumers that need exact backend values without RAF smoothing. */
export const rawResolvedChannelA: Readable<ChannelResolvedSnapshot> = { subscribe: targetA.subscribe };
export const rawResolvedChannelB: Readable<ChannelResolvedSnapshot> = { subscribe: targetB.subscribe };

/**
 * Convenience: returns true when this parameter slot is driven by an input
 * axis (Linked). Static parameters omit `source_axis` from the wire format.
 */
export function isLinkedSample(sample: ResolvedSampleSnapshot): boolean {
  return sample.source_axis !== undefined;
}

/**
 * Pick the right channel snapshot for a `'A' | 'B'` channel id.
 */
export function resolvedChannel(channel: 'A' | 'B'): Readable<ChannelResolvedSnapshot> {
  return channel === 'A' ? resolvedChannelA : resolvedChannelB;
}
