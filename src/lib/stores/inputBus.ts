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
 * smoothing; the bus store is raw and per-write so consumers that want
 * smoothing can RAF-interpolate themselves.
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

function handleBusUpdate(payload: BusUpdatePayload) {
  if (!isTracking) return;
  axes.update((current) => ({
    ...current,
    [payload.axis]: {
      value: payload.value,
      timestamp_ms: payload.timestamp_ms,
      interval_ms: payload.interval_ms
    }
  }));
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
}

/**
 * Drop every known axis. Used when the input protocol disconnects so a
 * reconnect doesn't carry over stale axis names from the previous
 * session.
 */
export function clearInputBus(): void {
  axes.set({});
}

/**
 * Drop axes whose name starts with `prefix`. Mirrors backend
 * `InputBus::clear_prefix` for the frontend's last-known-value Map.
 */
export function clearInputBusPrefix(prefix: string): void {
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
