<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import { channelA, channelB } from '$lib/stores/channels.js';
  import { inputPositionA, inputPositionB } from '$lib/stores/inputPosition.js';

  export let channel: 'A' | 'B';
  export let compact = false;
  export let showLabels = true;
  export let showWrapper = true;
  export let showPositionIndicator = true;

  // Get reactive store for this channel
  $: store = channel === 'A' ? channelA : channelB;
  $: inputPosition = channel === 'A' ? $inputPositionA : $inputPositionB;

  $: minValue = $store.rangeMin;
  $: maxValue = $store.rangeMax;

  // Calculate position indicator location within the min-max range
  // Input position (0-1) maps to the range between minPercent and maxPercent
  $: indicatorPercent = minPercent + (inputPosition * (maxPercent - minPercent));

  const dispatch = createEventDispatcher<{
    change: { min: number; max: number; range: number; maximum: number };
  }>();

  // Reactive calculations matching original Python implementation
  $: range = (maxValue - minValue) / 2;
  $: maximum = maxValue / 2;

  $: dispatch('change', {
    min: minValue,
    max: maxValue,
    range,
    maximum
  });

  // Percentage calculations for the track fill
  $: minPercent = (minValue / 200) * 100;
  $: maxPercent = (maxValue / 200) * 100;

  function handleMinInput(event: Event) {
    const target = event.target as HTMLInputElement;
    const newValue = Number(target.value);
    // Ensure min doesn't exceed max (keep 2-step gap for whole number display)
    if (newValue < maxValue) {
      store.update(s => ({ ...s, rangeMin: newValue }));
    } else {
      store.update(s => ({ ...s, rangeMin: maxValue - 2 }));
    }
  }

  function handleMaxInput(event: Event) {
    const target = event.target as HTMLInputElement;
    const newValue = Number(target.value);
    // Ensure max doesn't go below min (keep 2-step gap for whole number display)
    if (newValue > minValue) {
      store.update(s => ({ ...s, rangeMax: newValue }));
    } else {
      store.update(s => ({ ...s, rangeMax: minValue + 2 }));
    }
  }

  function handleWheel(event: WheelEvent) {
    event.preventDefault();
    const delta = event.deltaY < 0 ? 2 : -2;

    if (event.ctrlKey) {
      // Move minimum value only
      const newMin = Math.max(0, Math.min(maxValue - 1, minValue + delta));
      store.update(s => ({ ...s, rangeMin: newMin }));
    } else if (event.shiftKey) {
      // Move maximum value only
      const newMax = Math.max(minValue + 1, Math.min(200, maxValue + delta));
      store.update(s => ({ ...s, rangeMax: newMax }));
    } else {
      // Move entire range
      const rangeSize = maxValue - minValue;
      const newMin = Math.max(0, Math.min(200 - rangeSize, minValue + delta));
      const newMax = newMin + rangeSize;
      store.update(s => ({ ...s, rangeMin: newMin, rangeMax: newMax }));
    }
  }
</script>

<div class="{showWrapper ? (compact ? 'space-y-2' : 'bg-card border rounded-lg p-4') : 'space-y-1'}">
  <div class="{showWrapper ? (compact ? 'space-y-2' : 'space-y-4') : 'space-y-1'}">
    {#if showLabels}
    <!-- Labels -->
    <div class="flex {compact ? 'flex-col' : 'justify-between items-center'} text-xs">
      <span class="font-medium {channel === 'A' ? 'text-primary' : 'text-secondary'}">Ch {channel}</span>
      <div class="flex space-x-1 text-muted-foreground font-mono">
        <span>{Math.round(range)}%</span>
        <span class="text-muted">|</span>
        <span>{Math.round(maximum)}%</span>
      </div>
    </div>
    {/if}

    <!-- Dual Range Slider using native inputs -->
    <!-- svelte-ignore a11y-no-static-element-interactions -->
    <div
      class="range-slider-container relative w-full h-6"
      style="--min-percent: {minPercent}%; --max-percent: {maxPercent}%; --slider-color: hsl(var(--{channel === 'A' ? 'primary' : 'secondary'})); --slider-shadow-1: hsl(var(--{channel === 'A' ? 'primary' : 'secondary'}) / 0.2); --slider-shadow-2: hsl(var(--{channel === 'A' ? 'primary' : 'secondary'}) / 0.6); --slider-shadow-3: hsl(var(--{channel === 'A' ? 'primary' : 'secondary'}) / 0.8)"
      on:wheel={handleWheel}
    >
      <!-- Track background -->
      <div class="absolute top-1/2 -translate-y-1/2 left-0 right-0 h-3 bg-muted rounded-full pointer-events-none"></div>

      <!-- Active range highlight -->
      <div
        class="absolute top-1/2 -translate-y-1/2 h-3 rounded-full pointer-events-none {channel === 'A' ? 'bg-primary' : 'bg-secondary'}"
        style="left: {minPercent}%; width: {maxPercent - minPercent}%; box-shadow: 0 0 10px var(--slider-shadow-2)"
      ></div>

      <!-- Current input position indicator -->
      {#if showPositionIndicator && inputPosition > 0}
      <div
        class="position-indicator absolute top-1/2 pointer-events-none z-20"
        style="left: {indicatorPercent}%"
      >
        <!-- Outer glow (largest, most diffuse) -->
        <div
          class="absolute -translate-x-1/2 -translate-y-1/2 w-4 h-6 rounded-full blur-md opacity-40"
          style="background: var(--slider-color)"
        ></div>
        <!-- Middle glow -->
        <div
          class="absolute -translate-x-1/2 -translate-y-1/2 w-2 h-5 rounded-full blur-sm opacity-60"
          style="background: var(--slider-color)"
        ></div>
        <!-- Core line (bright center) -->
        <div
          class="absolute -translate-x-1/2 -translate-y-1/2 w-1 h-4 rounded-full"
          style="background: var(--slider-color); box-shadow: 0 0 8px var(--slider-shadow-2), 0 0 4px var(--slider-color)"
        ></div>
        <!-- White highlight for visibility -->
        <div class="absolute -translate-x-1/2 -translate-y-1/2 w-0.5 h-3 bg-white/70 rounded-full"></div>
      </div>
      {/if}

      <!-- Min range input -->
      <input
        type="range"
        min="0"
        max="200"
        step="2"
        value={minValue}
        on:input={handleMinInput}
        class="range-input range-input-min absolute top-1/2 -translate-y-1/2 w-full h-3 appearance-none bg-transparent cursor-pointer"
      />

      <!-- Max range input -->
      <input
        type="range"
        min="0"
        max="200"
        step="2"
        value={maxValue}
        on:input={handleMaxInput}
        class="range-input range-input-max absolute top-1/2 -translate-y-1/2 w-full h-3 appearance-none bg-transparent cursor-pointer"
      />
    </div>
  </div>
</div>

<style>
  .range-slider-container {
    touch-action: none;
  }

  .range-input {
    pointer-events: none;
    margin: 0;
    padding: 0;
  }

  .range-input::-webkit-slider-thumb {
    pointer-events: auto;
    appearance: none;
    width: 24px;
    height: 24px;
    background: var(--slider-color, hsl(var(--primary)));
    border: 3px solid hsl(var(--background));
    border-radius: 50%;
    cursor: pointer;
    box-shadow: 0 0 0 1px var(--slider-shadow-1, hsl(var(--primary) / 0.2)), 0 0 15px var(--slider-shadow-2, hsl(var(--primary) / 0.6));
    transition: box-shadow 0.2s ease, transform 0.2s ease;
    position: relative;
    z-index: 1;
  }

  .range-input::-webkit-slider-thumb:hover {
    box-shadow: 0 0 0 6px var(--slider-shadow-1, hsl(var(--primary) / 0.2)), 0 0 25px var(--slider-shadow-3, hsl(var(--primary) / 0.8));
    transform: scale(1.1);
  }

  .range-input::-webkit-slider-thumb:active {
    transform: scale(0.95);
  }

  .range-input::-moz-range-thumb {
    pointer-events: auto;
    width: 24px;
    height: 24px;
    background: var(--slider-color, hsl(var(--primary)));
    border: 3px solid hsl(var(--background));
    border-radius: 50%;
    cursor: pointer;
    box-shadow: 0 0 0 1px var(--slider-shadow-1, hsl(var(--primary) / 0.2)), 0 0 15px var(--slider-shadow-2, hsl(var(--primary) / 0.6));
    transition: box-shadow 0.2s ease, transform 0.2s ease;
    position: relative;
    z-index: 1;
  }

  .range-input::-moz-range-thumb:hover {
    box-shadow: 0 0 0 6px var(--slider-shadow-1, hsl(var(--primary) / 0.2)), 0 0 25px var(--slider-shadow-3, hsl(var(--primary) / 0.8));
    transform: scale(1.1);
  }

  .range-input::-moz-range-thumb:active {
    transform: scale(0.95);
  }

  .range-input::-webkit-slider-runnable-track {
    background: transparent;
  }

  .range-input::-moz-range-track {
    background: transparent;
  }

  .range-input:focus {
    outline: none;
  }

  .range-input:focus-visible::-webkit-slider-thumb {
    box-shadow: 0 0 0 3px hsl(var(--background)), 0 0 0 5px hsl(var(--ring));
  }

  .range-input:focus-visible::-moz-range-thumb {
    box-shadow: 0 0 0 3px hsl(var(--background)), 0 0 0 5px hsl(var(--ring));
  }

  /* Ensure max thumb is above min when they overlap */
  .range-input-max::-webkit-slider-thumb {
    z-index: 2;
  }

  .range-input-max::-moz-range-thumb {
    z-index: 2;
  }
</style>
