<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import { type ScalarInput, isAxisInput } from '$lib/types/modulation.js';
  import Slider from './Slider.svelte';

  /**
   * Editor for a transform's `ScalarInput` modifier (speed / direction /
   * amount). Toggles between a **constant** (the default — a slider, or a
   * CW/CCW pair when `control === 'toggle'`) and a **linked bus axis**
   * (a dropdown of known axes). Replaces the old required axis text box:
   * a modifier is never invalid out of the box because it starts as a
   * constant.
   *
   * Emits `change` with the new `ScalarInput` (a `number` for constant, a
   * `string` for axis). The parent merges it back into the transform.
   */

  export let value: ScalarInput;
  export let label: string;
  export let channel: 'A' | 'B';
  /** 'slider' = 0..1 magnitude; 'toggle' = directional CW/CCW pair. */
  export let control: 'slider' | 'toggle' = 'slider';
  export let min = 0;
  export let max = 1;
  export let step = 0.05;
  /** Value to seed when the user switches from axis → constant. */
  export let constFallback = 0.5;
  /** Known bus axes for the dropdown (from `inputBus.knownAxes`). */
  export let axes: readonly string[] = [];

  const dispatch = createEventDispatcher<{ change: ScalarInput }>();

  $: linked = isAxisInput(value);
  $: constValue = linked ? constFallback : (value as number);
  $: axisValue = linked ? (value as string) : '';
  $: variant = (channel === 'A' ? 'primary' : 'secondary') as 'primary' | 'secondary';

  // Include the current axis in the option list even if the bus hasn't
  // reported it yet this session, so a saved-but-currently-silent axis
  // (e.g. a Buttplug feature not connected right now) still shows.
  $: options =
    axisValue && !axes.includes(axisValue) ? [axisValue, ...axes] : [...axes];

  function useConst() {
    if (!linked) return;
    dispatch('change', constFallback);
  }

  function useAxis() {
    if (linked) return;
    // Default to the first known axis if there is one, else empty.
    dispatch('change', axes[0] ?? '');
  }
</script>

<div class="space-y-0.5">
  <div class="flex items-center justify-between gap-2 text-[10px] text-muted-foreground">
    <span>{label}</span>
    <div class="flex items-center gap-1">
      {#if !linked && control === 'slider'}
        <span class="font-mono">{constValue.toFixed(2)}</span>
      {/if}
      <!-- Const / Axis segmented toggle -->
      <div class="inline-flex rounded border border-border overflow-hidden">
        <button
          type="button"
          class="px-1.5 py-0.5 text-[10px] {!linked
            ? 'bg-muted text-foreground'
            : 'text-muted-foreground hover:text-foreground'}"
          on:click={useConst}
        >
          Value
        </button>
        <button
          type="button"
          class="px-1.5 py-0.5 text-[10px] border-l border-border {linked
            ? 'bg-muted text-foreground'
            : 'text-muted-foreground hover:text-foreground'}"
          on:click={useAxis}
        >
          Axis
        </button>
      </div>
    </div>
  </div>

  {#if linked}
    <select
      value={axisValue}
      on:change={(e) => dispatch('change', e.currentTarget.value)}
      class="w-full px-1 py-0.5 text-xs font-mono rounded border border-border bg-background text-foreground"
    >
      {#if options.length === 0}
        <option value="" disabled selected>No axes seen yet</option>
      {:else}
        {#if !axisValue}
          <option value="" disabled selected>Select axis…</option>
        {/if}
        {#each options as axis (axis)}
          <option value={axis}>{axis}</option>
        {/each}
      {/if}
    </select>
  {:else if control === 'toggle'}
    <div class="inline-flex rounded border border-border overflow-hidden text-xs">
      <button
        type="button"
        class="px-2 py-0.5 {constValue >= 0.5
          ? 'bg-muted text-foreground'
          : 'text-muted-foreground hover:text-foreground'}"
        on:click={() => dispatch('change', 1)}
      >
        ↻ CW
      </button>
      <button
        type="button"
        class="px-2 py-0.5 border-l border-border {constValue < 0.5
          ? 'bg-muted text-foreground'
          : 'text-muted-foreground hover:text-foreground'}"
        on:click={() => dispatch('change', 0)}
      >
        ↺ CCW
      </button>
    </div>
  {:else}
    <Slider
      value={constValue}
      {min}
      {max}
      {step}
      {variant}
      on:change={(e) => dispatch('change', e.detail)}
      class="h-3"
    />
  {/if}
</div>
