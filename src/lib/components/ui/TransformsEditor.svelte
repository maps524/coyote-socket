<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import { ArrowUp, ArrowDown, X, Plus } from 'lucide-svelte';
  import {
    type Transform,
    TRANSFORM_TYPES,
    TRANSFORM_LABELS,
    defaultTransform
  } from '$lib/types/modulation.js';
  import Slider from './Slider.svelte';

  /**
   * Per-parameter transforms editor (sub G.2).
   *
   * Edits a `ParameterSource.transforms` vector in place: list / add /
   * reorder / delete. Posts the updated array via the `change` event;
   * the parent (`RangeSliderWithIndicator`) merges it back into the
   * `ParameterSource` and dispatches `sourceChange`. The new transforms
   * round-trip into the backend via `apply_channel_config`.
   *
   * Variant editors are inlined here rather than per-component since the
   * row layout is tight and per-variant code is short. If/when a
   * variant grows enough that this file is hard to scan, factor out
   * `TransformRowVibrate.svelte` etc. and route through the
   * `dispatchUpdate` pattern.
   */

  export let channel: 'A' | 'B';
  export let transforms: Transform[] = [];

  const dispatch = createEventDispatcher<{ change: Transform[] }>();

  // Variant picker for the "+ Add" dropdown. Defaults to the first
  // variant; users pick any variant from `TRANSFORM_TYPES`.
  let pendingType: Transform['type'] = 'smooth';

  function emit(next: Transform[]) {
    dispatch('change', next);
  }

  function addTransform() {
    emit([...transforms, defaultTransform(pendingType)]);
  }

  function deleteAt(i: number) {
    emit(transforms.filter((_, j) => j !== i));
  }

  function moveUp(i: number) {
    if (i <= 0) return;
    const next = transforms.slice();
    [next[i - 1], next[i]] = [next[i], next[i - 1]];
    emit(next);
  }

  function moveDown(i: number) {
    if (i >= transforms.length - 1) return;
    const next = transforms.slice();
    [next[i], next[i + 1]] = [next[i + 1], next[i]];
    emit(next);
  }

  function updateAt(i: number, t: Transform) {
    emit(transforms.map((x, j) => (j === i ? t : x)));
  }

  // Convenience: per-field setter that preserves the rest of the variant
  // payload. Generic over the discriminator so changing one field of a
  // Vibrate doesn't accidentally widen the variant.
  function set<T extends Transform, K extends keyof T>(
    i: number,
    transform: T,
    key: K,
    value: T[K]
  ) {
    updateAt(i, { ...transform, [key]: value } as Transform);
  }

  function num(value: string, fallback: number): number {
    const n = Number(value);
    return Number.isFinite(n) ? n : fallback;
  }

  // Tailwind class palette — keep label / button styling consistent
  // with the surrounding popover so the editor doesn't feel grafted on.
  $: variantTone = channel === 'A' ? 'text-primary' : 'text-secondary';
</script>

<div class="space-y-1.5 pt-2 border-t border-border/50">
  <div class="flex items-center justify-between">
    <span class="text-[10px] uppercase tracking-wide text-muted-foreground">Transforms</span>
    <span class="text-[10px] font-mono text-muted-foreground">{transforms.length}</span>
  </div>

  {#each transforms as t, i (i)}
    <div class="rounded border border-border/50 bg-muted/30 px-1.5 py-1 space-y-1">
      <!-- Header: reorder + label + delete -->
      <div class="flex items-center gap-1">
        <button
          type="button"
          class="p-0.5 text-muted-foreground hover:text-foreground disabled:opacity-30"
          disabled={i === 0}
          aria-label="Move up"
          on:click={() => moveUp(i)}
        >
          <ArrowUp class="h-3 w-3" />
        </button>
        <button
          type="button"
          class="p-0.5 text-muted-foreground hover:text-foreground disabled:opacity-30"
          disabled={i === transforms.length - 1}
          aria-label="Move down"
          on:click={() => moveDown(i)}
        >
          <ArrowDown class="h-3 w-3" />
        </button>
        <span class="text-xs font-medium {variantTone}">{TRANSFORM_LABELS[t.type]}</span>
        <button
          type="button"
          class="ml-auto p-0.5 text-muted-foreground hover:text-destructive"
          aria-label="Delete transform"
          on:click={() => deleteAt(i)}
        >
          <X class="h-3 w-3" />
        </button>
      </div>

      <!-- Per-variant fields. Layout is intentionally flat — the popover
           is narrow and a tabular form would overflow. -->
      {#if t.type === 'smooth'}
        <label class="flex items-center justify-between gap-2 text-[10px] text-muted-foreground">
          <span>Time constant (ms)</span>
          <input
            type="number"
            min="1"
            step="10"
            value={t.time_constant_ms}
            on:input={(e) => set(i, t, 'time_constant_ms', num(e.currentTarget.value, t.time_constant_ms))}
            class="w-16 px-1 py-0.5 text-xs rounded border border-border bg-background text-foreground"
          />
        </label>
      {:else if t.type === 'scale'}
        <label class="flex items-center justify-between gap-2 text-[10px] text-muted-foreground">
          <span>Factor</span>
          <input
            type="number"
            step="0.1"
            value={t.factor}
            on:input={(e) => set(i, t, 'factor', num(e.currentTarget.value, t.factor))}
            class="w-16 px-1 py-0.5 text-xs rounded border border-border bg-background text-foreground"
          />
        </label>
      {:else if t.type === 'clamp'}
        <div class="flex items-center justify-between gap-2 text-[10px] text-muted-foreground">
          <span>Min</span>
          <input
            type="number"
            step="0.05"
            value={t.min}
            on:input={(e) => set(i, t, 'min', num(e.currentTarget.value, t.min))}
            class="w-14 px-1 py-0.5 text-xs rounded border border-border bg-background text-foreground"
          />
          <span>Max</span>
          <input
            type="number"
            step="0.05"
            value={t.max}
            on:input={(e) => set(i, t, 'max', num(e.currentTarget.value, t.max))}
            class="w-14 px-1 py-0.5 text-xs rounded border border-border bg-background text-foreground"
          />
        </div>
      {:else if t.type === 'invert'}
        <span class="text-[10px] text-muted-foreground">No parameters</span>
      {:else if t.type === 'hold'}
        <label class="flex items-center justify-between gap-2 text-[10px] text-muted-foreground">
          <span>Duration (ms)</span>
          <input
            type="number"
            min="1"
            step="10"
            value={t.duration_ms}
            on:input={(e) => set(i, t, 'duration_ms', num(e.currentTarget.value, t.duration_ms))}
            class="w-16 px-1 py-0.5 text-xs rounded border border-border bg-background text-foreground"
          />
        </label>
      {:else if t.type === 'mix'}
        <label class="flex items-center justify-between gap-2 text-[10px] text-muted-foreground">
          <span>Other axis</span>
          <input
            type="text"
            value={t.otherAxis}
            placeholder="L1"
            on:input={(e) => set(i, t, 'otherAxis', e.currentTarget.value)}
            class="w-20 px-1 py-0.5 text-xs font-mono rounded border border-border bg-background text-foreground"
          />
        </label>
        <div class="space-y-0.5">
          <div class="flex justify-between text-[10px] text-muted-foreground">
            <span>Weight</span>
            <span class="font-mono">{t.weight.toFixed(2)}</span>
          </div>
          <Slider
            value={t.weight}
            min={0}
            max={1}
            step={0.05}
            variant={channel === 'A' ? 'primary' : 'secondary'}
            on:change={(e) => set(i, t, 'weight', e.detail)}
            class="h-3"
          />
        </div>
      {:else if t.type === 'vibrate'}
        <label class="flex items-center justify-between gap-2 text-[10px] text-muted-foreground">
          <span>Speed axis</span>
          <input
            type="text"
            value={t.speedAxis}
            placeholder="bp:Vibrate_0"
            on:input={(e) => set(i, t, 'speedAxis', e.currentTarget.value)}
            class="w-24 px-1 py-0.5 text-xs font-mono rounded border border-border bg-background text-foreground"
          />
        </label>
        <div class="space-y-0.5">
          <div class="flex justify-between text-[10px] text-muted-foreground">
            <span>Distance</span>
            <span class="font-mono">{t.distance.toFixed(2)}</span>
          </div>
          <Slider
            value={t.distance}
            min={0}
            max={1}
            step={0.05}
            variant={channel === 'A' ? 'primary' : 'secondary'}
            on:change={(e) => set(i, t, 'distance', e.detail)}
            class="h-3"
          />
        </div>
      {:else if t.type === 'oscillate'}
        <label class="flex items-center justify-between gap-2 text-[10px] text-muted-foreground">
          <span>Speed axis</span>
          <input
            type="text"
            value={t.speedAxis}
            placeholder="bp:Oscillate_0"
            on:input={(e) => set(i, t, 'speedAxis', e.currentTarget.value)}
            class="w-24 px-1 py-0.5 text-xs font-mono rounded border border-border bg-background text-foreground"
          />
        </label>
        <div class="space-y-0.5">
          <div class="flex justify-between text-[10px] text-muted-foreground">
            <span>Scale</span>
            <span class="font-mono">{t.scale.toFixed(2)}</span>
          </div>
          <Slider
            value={t.scale}
            min={0}
            max={1}
            step={0.05}
            variant={channel === 'A' ? 'primary' : 'secondary'}
            on:change={(e) => set(i, t, 'scale', e.detail)}
            class="h-3"
          />
        </div>
        <label class="flex items-center justify-between gap-2 text-[10px] text-muted-foreground">
          <span>Max speed (Hz)</span>
          <input
            type="number"
            step="0.5"
            min="0.1"
            value={t.maxSpeedHz}
            on:input={(e) => set(i, t, 'maxSpeedHz', num(e.currentTarget.value, t.maxSpeedHz))}
            class="w-14 px-1 py-0.5 text-xs rounded border border-border bg-background text-foreground"
          />
        </label>
      {:else if t.type === 'rotate'}
        <label class="flex items-center justify-between gap-2 text-[10px] text-muted-foreground">
          <span>Speed axis</span>
          <input
            type="text"
            value={t.speedAxis}
            placeholder="bp:Rotate_0"
            on:input={(e) => set(i, t, 'speedAxis', e.currentTarget.value)}
            class="w-24 px-1 py-0.5 text-xs font-mono rounded border border-border bg-background text-foreground"
          />
        </label>
        <label class="flex items-center justify-between gap-2 text-[10px] text-muted-foreground">
          <span>Direction axis</span>
          <input
            type="text"
            value={t.directionAxis}
            placeholder="bp:RotateDir_0"
            on:input={(e) => set(i, t, 'directionAxis', e.currentTarget.value)}
            class="w-24 px-1 py-0.5 text-xs font-mono rounded border border-border bg-background text-foreground"
          />
        </label>
        <div class="space-y-0.5">
          <div class="flex justify-between text-[10px] text-muted-foreground">
            <span>Scale</span>
            <span class="font-mono">{t.scale.toFixed(2)}</span>
          </div>
          <Slider
            value={t.scale}
            min={0}
            max={1}
            step={0.05}
            variant={channel === 'A' ? 'primary' : 'secondary'}
            on:change={(e) => set(i, t, 'scale', e.detail)}
            class="h-3"
          />
        </div>
        <label class="flex items-center justify-between gap-2 text-[10px] text-muted-foreground">
          <span>Max speed (Hz)</span>
          <input
            type="number"
            step="0.5"
            min="0.1"
            value={t.maxSpeedHz}
            on:input={(e) => set(i, t, 'maxSpeedHz', num(e.currentTarget.value, t.maxSpeedHz))}
            class="w-14 px-1 py-0.5 text-xs rounded border border-border bg-background text-foreground"
          />
        </label>
      {:else if t.type === 'constrict'}
        <label class="flex items-center justify-between gap-2 text-[10px] text-muted-foreground">
          <span>Amount axis</span>
          <input
            type="text"
            value={t.amountAxis}
            placeholder="bp:Constrict_0"
            on:input={(e) => set(i, t, 'amountAxis', e.currentTarget.value)}
            class="w-24 px-1 py-0.5 text-xs font-mono rounded border border-border bg-background text-foreground"
          />
        </label>
        <div class="space-y-0.5">
          <div class="flex justify-between text-[10px] text-muted-foreground">
            <span>Min floor</span>
            <span class="font-mono">{t.minFloor.toFixed(2)}</span>
          </div>
          <Slider
            value={t.minFloor}
            min={0}
            max={1}
            step={0.05}
            variant={channel === 'A' ? 'primary' : 'secondary'}
            on:change={(e) => set(i, t, 'minFloor', e.detail)}
            class="h-3"
          />
        </div>
        <label class="flex items-center justify-between text-[10px] text-muted-foreground">
          <span>Method</span>
          <select
            value={t.method}
            on:change={(e) => set(i, t, 'method', e.currentTarget.value === 'Clamp' ? 'Clamp' : 'Downsample')}
            class="px-1 py-0.5 text-xs rounded border border-border bg-background text-foreground"
          >
            <option value="Downsample">Downsample</option>
            <option value="Clamp">Clamp</option>
          </select>
        </label>
        <label class="flex items-center justify-between text-[10px] text-muted-foreground cursor-pointer">
          <span>Use midpoint</span>
          <input
            type="checkbox"
            checked={t.useMidpoint}
            on:change={(e) => set(i, t, 'useMidpoint', e.currentTarget.checked)}
            class="w-3.5 h-3.5 rounded border-border bg-background text-primary focus:ring-primary focus:ring-offset-0"
          />
        </label>
      {/if}
    </div>
  {/each}

  <!-- Add control: variant picker + add button -->
  <div class="flex items-center gap-1 pt-1">
    <select
      bind:value={pendingType}
      class="flex-1 px-1.5 py-1 text-xs rounded border border-border bg-background text-foreground"
    >
      {#each TRANSFORM_TYPES as type (type)}
        <option value={type}>{TRANSFORM_LABELS[type]}</option>
      {/each}
    </select>
    <button
      type="button"
      class="inline-flex items-center gap-0.5 px-2 py-1 text-xs rounded bg-muted hover:bg-muted/70 text-foreground"
      on:click={addTransform}
    >
      <Plus class="h-3 w-3" />
      Add
    </button>
  </div>
</div>
