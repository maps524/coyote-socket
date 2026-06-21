/**
 * Input Bus Store (sub G.3)
 *
 * Subscribes to the backend `bus-update` Tauri event — fired per-write
 * inside `ProcessingState::bus_write`, source-tagged by axis name prefix
 * (T-Code: bare `L0`/`R2`/...; gamepad: `GP_*`; buttplug or lovense:
 * `bp:*`). Replaces the batched `axis-update` and `buttplug-features`
 * events that sub G.3 retired.
 *
 * The store carries a flat `Map<axis, AxisValue>` so consumers can either
 * read a single axis or iterate every known axis. The InputMonitor
 * partitions by prefix; the transforms editor's axis discovery dropdown
 * (G.3.2) lists every observed axis. RAF smoothing isn't applied here —
 * sub G.1's `resolvedState.ts` is the post-curve view that needs
 * smoothing; the bus store is raw so consumers that want smoothing can
 * RAF-interpolate themselves.
 *
 * **Writes are rAF-coalesced.** `bus-update` fires once per axis write —
 * tens to hundreds per second at Buttplug/T-Code cadence. Pushing each
 * one straight into the store made every subscriber (the `knownAxes`
 * re-sort, the InputMonitor recompute, all 8 `RangeSliderWithIndicator`
 * axis-group derivations) re-run per event, so the UI got progressively
 * laggier the longer a session ran (and a refresh "fixed" it by resetting
 * the in-memory churn). We now buffer incoming writes keyed by axis and
 * flush the batch into the store at most once per animation frame, so
 * every consumer updates once per painted frame regardless of input
 * cadence. The buffer is keyed by axis name, so it's bounded by the axis
 * count (~20), and a later write to the same axis within a frame simply
 * overwrites the earlier one — only the freshest value per axis is kept.
 * Coalescing is purely a render concern; the device output is computed in
 * the backend and never touches this store.
 */
import { writable, derived, type Readable } from 'svelte/store';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

export interface AxisValue {
  value: number;          // raw 0..1 sample as written to the bus
  timestamp_ms: number;   // monotonic ms from backend
  interval_ms?: number;   // T-Code ramp / Buttplug LinearCmd duration; absent for instantaneous writes
}

interface BusUpdatePayload {
  axis: string;
  value: number;
  timestamp_ms: number;
  interval_ms?: number;
}

const axes = writable<Record<string, AxisValue>>({});

let unlistenFn: UnlistenFn | null = null;
let isTracking = false;

// rAF-coalescing buffer: writes accumulate here keyed by axis and flush to
// the store once per frame. `null` means no writes are pending.
let pending: Record<string, AxisValue> | null = null;
let rafId: number | null = null;

function flushPending(): void {
  rafId = null;
  if (pending === null) return;
  const batch = pending;
  pending = null;
  axes.update((current) => ({ ...current, ...batch }));
}

function scheduleFlush(): void {
  if (rafId !== null) return; // a frame is already queued — coalesce into it
  if (typeof requestAnimationFrame === 'undefined') {
    // Non-browser fallback (e.g. test/SSR): apply immediately.
    flushPending();
    return;
  }
  rafId = requestAnimationFrame(flushPending);
}

function handleBusUpdate(payload: BusUpdatePayload) {
  if (!isTracking) return;
  if (pending === null) pending = {};
  // Last write per axis within the frame wins.
  pending[payload.axis] = {
    value: payload.value,
    timestamp_ms: payload.timestamp_ms,
    interval_ms: payload.interval_ms
  };
  scheduleFlush();
}

/**
 * Begin listening for `bus-update` events. Idempotent.
 */
export async function startInputBusTracking(): Promise<void> {
  if (isTracking) return;
  isTracking = true;
  try {
    unlistenFn = await listen<BusUpdatePayload>('bus-update', (event) => {
      handleBusUpdate(event.payload);
    });
  } catch (e) {
    console.error('Failed to listen for bus-update events:', e);
  }
}

/**
 * Stop the listener. Safe to call when not running.
 */
export function stopInputBusTracking(): void {
  isTracking = false;
  if (unlistenFn !== null) {
    unlistenFn();
    unlistenFn = null;
  }
  // Drop any queued frame + buffered writes so a stopped listener can't
  // flush stale data after teardown.
  if (rafId !== null && typeof cancelAnimationFrame !== 'undefined') {
    cancelAnimationFrame(rafId);
  }
  rafId = null;
  pending = null;
}

/**
 * Drop every known axis. Used when the input protocol disconnects so a
 * reconnect doesn't carry over stale axis names from the previous
 * session.
 */
export function clearInputBus(): void {
  // Drop buffered writes too — a clear supersedes anything queued this frame.
  pending = null;
  axes.set({});
}

/**
 * Drop axes whose name starts with `prefix`. Mirrors backend
 * `InputBus::clear_prefix` for the frontend's last-known-value Map.
 */
export function clearInputBusPrefix(prefix: string): void {
  // Also drop matching buffered writes so they don't repopulate after clear.
  if (pending !== null) {
    for (const k of Object.keys(pending)) {
      if (k.startsWith(prefix)) delete pending[k];
    }
  }
  axes.update((current) => {
    const next: Record<string, AxisValue> = {};
    for (const [k, v] of Object.entries(current)) {
      if (!k.startsWith(prefix)) next[k] = v;
    }
    return next;
  });
}

/** Flat readable view of every known axis on the bus, keyed by axis name. */
export const inputBus: Readable<Record<string, AxisValue>> = { subscribe: axes.subscribe };

/** Sorted list of every known axis name. Refreshes whenever the map changes. */
export const knownAxes: Readable<string[]> = derived(axes, ($axes) =>
  Object.keys($axes).sort((a, b) => a.localeCompare(b))
);
