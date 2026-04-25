<script lang="ts">
  import { createEventDispatcher, onDestroy } from 'svelte';
  import type { ParameterSource, CurveType } from '$lib/types/modulation.js';
  import { Info, Link, MapPin, Clock, RotateCw, MoveHorizontal, Activity, Minimize2 } from 'lucide-svelte';
  import Tooltip from './Tooltip.svelte';
  import Slider from './Slider.svelte';
  import Popover from './Popover.svelte';
  import ButtplugLinkPanel from './ButtplugLinkPanel.svelte';

  // Props
  export let channel: 'A' | 'B';
  export let parameterName: string = 'Parameter';
  export let source: ParameterSource;
  export let indicatorValue: number = 0; // 0-1 normalized input position
  export let min: number = 0;
  export let max: number = 200;
  export let step: number = 2;
  export let compact: boolean = false;
  export let showLabels: boolean = true;
  export let showWrapper: boolean = true;
  export let tooltip: string = ''; // Optional tooltip text
  export let isIntensity: boolean = false; // Special handling for intensity display
  export let wheelStep: ((currentValue: number, direction: 'up' | 'down') => number) | undefined = undefined;
  export let inputMode: 'tcode' | 'buttplug' | 'none' = 'tcode'; // Input source mode

  const dispatch = createEventDispatcher<{
    sourceChange: ParameterSource;
    rangeChange: { min: number; max: number };
  }>();

  // Axis button grid layout. T-Code rows + gamepad row.
  const axisRows = [
    ['L0', 'L1', 'L2'],
    ['R0', 'R1', 'R2']
  ];
  const gamepadAxisRows = [
    ['GP_LX', 'GP_LY', 'GP_LT'],
    ['GP_RX', 'GP_RY', 'GP_RT']
  ];

  // Curve options for dropdown
  const curveOptions: { value: CurveType; label: string }[] = [
    { value: 'linear', label: 'Linear' },
    { value: 'exponential', label: 'Exponential' },
    { value: 'logarithmic', label: 'Logarithmic' },
    { value: 's-curve', label: 'S-Curve' },
    { value: 'inverse', label: 'Inverse' }
  ];

  // Local values for range handles
  // Always read from source to preserve both staticValue AND rangeMin/Max when switching modes
  let minValue = source.rangeMin ?? min;
  let maxValue = source.rangeMax ?? max;
  let staticValue = source.staticValue ?? 100;

  // Source selection
  let selectedSource = source.type === 'static' ? 'static' : (source.sourceAxis ?? 'L0');
  let selectedCurve: CurveType = source.curve ?? 'linear';
  let curveStrength: number = source.curveStrength ?? 2.0;
  let midpointEnabled: boolean = source.midpoint ?? false;

  // Does the selected curve support strength adjustment?
  $: curveSupportsStrength = selectedCurve === 'exponential' || selectedCurve === 'logarithmic';

  // Popover state
  let popoverOpen = false;

  // Update local values when source prop changes
  // Always sync all values from source to preserve them when switching modes
  $: {
    minValue = source.rangeMin ?? min;
    maxValue = source.rangeMax ?? max;
    staticValue = source.staticValue ?? staticValue; // Keep current if not in source
    selectedCurve = source.curve ?? 'linear';
    curveStrength = source.curveStrength ?? 2.0;
    midpointEnabled = source.midpoint ?? false;

    if (source.type === 'linked') {
      selectedSource = source.sourceAxis ?? 'L0';
    } else {
      selectedSource = 'static';
    }
  }

  // Calculate percentage positions for range handles
  $: minPercent = (minValue / max) * 100;
  $: maxPercent = (maxValue / max) * 100;

  // Calculate indicator position within the min-max range
  $: indicatorPercent = minPercent + (indicatorValue * (maxPercent - minPercent));

  // Is linked mode active? Depends on active input ecosystem
  // - In TCode mode: check if a TCode axis is linked
  // - In Buttplug mode: check if any Buttplug features are linked
  $: isLinked = inputMode === 'buttplug'
    ? hasButtplugLinks
    : source.type === 'linked';

  // Feature type icons mapping
  const featureIcons: Record<string, any> = {
    Position: MapPin,
    PositionWithDuration: Clock,
    Rotate: RotateCw,
    Oscillate: MoveHorizontal,
    Vibrate: Activity,
    Constrict: Minimize2
  };

  // Reactive: Get buttplug link items for display (icon + number pairs)
  // This is reactive so it updates when source.buttplugLinks changes
  $: buttplugLinkItems = (() => {
    const links = source.buttplugLinks;
    if (!links) return [];

    const items: Array<{ icon: any; index: number }> = [];
    if (links.position) {
      items.push({
        icon: featureIcons[links.position.featureType],
        index: links.position.featureIndex + 1
      });
    }
    if (links.motion) {
      items.push({
        icon: featureIcons[links.motion.featureType],
        index: links.motion.featureIndex + 1
      });
    }
    if (links.vibrate) {
      items.push({
        icon: featureIcons.Vibrate,
        index: links.vibrate.featureIndex + 1
      });
    }
    if (links.constrict) {
      items.push({
        icon: featureIcons.Constrict,
        index: links.constrict.featureIndex + 1
      });
    }
    return items;
  })();

  // Check if buttplug links are set (derived from buttplugLinkItems)
  $: hasButtplugLinks = buttplugLinkItems.length > 0;

  // Format value for display
  function formatValue(value: number): string {
    if (parameterName.toLowerCase().includes('frequency') && !parameterName.toLowerCase().includes('balance')) {
      const period = Math.round(1000 / value);
      const actualFreq = 1000 / period;
      return `${actualFreq.toFixed(1)} Hz`;
    } else if (isIntensity) {
      return `${Math.round(value / 2)}%`;
    } else {
      return `${Math.round(value)}`;
    }
  }

  // Format intensity range display: "{distance} | {max}"
  function formatIntensityRange(minVal: number, maxVal: number): string {
    const distance = (maxVal - minVal) / 2;
    const maximum = maxVal / 2;
    return `${Math.round(distance)}% | ${Math.round(maximum)}%`;
  }

  // Handle axis button click (radio button behavior - click again to deselect)
  function handleAxisClick(axis: string) {
    if (selectedSource === axis) {
      // Clicking the already-selected axis deselects it (return to static)
      // Include both staticValue AND range values to preserve both when switching
      dispatch('sourceChange', {
        type: 'static',
        staticValue: staticValue,
        rangeMin: minValue,
        rangeMax: maxValue,
        curve: selectedCurve,
        midpoint: midpointEnabled
      });
    } else {
      // Select this axis
      // Include staticValue to preserve it when switching back to static later
      dispatch('sourceChange', {
        type: 'linked',
        sourceAxis: axis,
        staticValue: staticValue,  // Preserve static value for when user switches back
        rangeMin: minValue,
        rangeMax: maxValue,
        curve: selectedCurve,
        curveStrength: curveStrength,
        midpoint: midpointEnabled
      });
    }
  }

  // Handle Buttplug link changes
  function handleButtplugLinkChange(event: CustomEvent<ParameterSource>) {
    dispatch('sourceChange', event.detail);
  }

  // Handle curve selection change
  function handleCurveChange(event: Event) {
    const target = event.target as HTMLSelectElement;
    selectedCurve = target.value as CurveType;

    // Always include all values to preserve state
    dispatch('sourceChange', {
      ...source,
      staticValue: staticValue,
      rangeMin: minValue,
      rangeMax: maxValue,
      curve: selectedCurve,
      curveStrength: curveStrength,
      midpoint: midpointEnabled
    });
  }

  // Handle curve strength change
  function handleStrengthChange(event: CustomEvent<number>) {
    curveStrength = event.detail;

    dispatch('sourceChange', {
      ...source,
      staticValue: staticValue,
      rangeMin: minValue,
      rangeMax: maxValue,
      curve: selectedCurve,
      curveStrength: curveStrength,
      midpoint: midpointEnabled
    });
  }

  // Handle midpoint toggle change
  function handleMidpointChange(event: Event) {
    const target = event.target as HTMLInputElement;
    midpointEnabled = target.checked;

    dispatch('sourceChange', {
      ...source,
      staticValue: staticValue,
      rangeMin: minValue,
      rangeMax: maxValue,
      curve: selectedCurve,
      curveStrength: curveStrength,
      midpoint: midpointEnabled
    });
  }

  // Handle static value change
  function handleStaticInput(event: Event) {
    const target = event.target as HTMLInputElement;
    const newValue = Number(target.value);
    staticValue = newValue;

    // Always include range values to preserve them
    dispatch('sourceChange', {
      ...source,
      staticValue: newValue,
      rangeMin: minValue,
      rangeMax: maxValue,
      midpoint: midpointEnabled
    });
  }

  // Handle min range change
  function handleMinInput(event: Event) {
    const target = event.target as HTMLInputElement;
    const newValue = Number(target.value);

    if (newValue < maxValue) {
      minValue = newValue;
    } else {
      minValue = maxValue - step;
    }

    dispatch('rangeChange', { min: minValue, max: maxValue });

    // Always include staticValue to preserve it
    dispatch('sourceChange', {
      ...source,
      staticValue: staticValue,
      rangeMin: minValue,
      rangeMax: maxValue,
      midpoint: midpointEnabled
    });
  }

  // Handle max range change
  function handleMaxInput(event: Event) {
    const target = event.target as HTMLInputElement;
    const newValue = Number(target.value);

    if (newValue > minValue) {
      maxValue = newValue;
    } else {
      maxValue = minValue + step;
    }

    dispatch('rangeChange', { min: minValue, max: maxValue });

    // Always include staticValue to preserve it
    dispatch('sourceChange', {
      ...source,
      staticValue: staticValue,
      rangeMin: minValue,
      rangeMax: maxValue,
      midpoint: midpointEnabled
    });
  }

  // Mouse wheel handling for static slider
  function handleStaticWheel(event: WheelEvent) {
    if (source.type !== 'static') return;
    event.preventDefault();

    const direction = event.deltaY < 0 ? 'up' : 'down';
    let newValue: number;

    if (wheelStep) {
      newValue = wheelStep(staticValue, direction);
    } else {
      const delta = direction === 'up' ? step : -step;
      newValue = staticValue + delta;
    }

    newValue = Math.max(min, Math.min(max, newValue));

    if (newValue !== staticValue) {
      staticValue = newValue;
      // Include range values to preserve them
      dispatch('sourceChange', {
        ...source,
        staticValue: newValue,
        rangeMin: minValue,
        rangeMax: maxValue,
        midpoint: midpointEnabled
      });
    }
  }

  // Mouse wheel handling for range adjustment
  function handleRangeWheel(event: WheelEvent) {
    event.preventDefault();
    const delta = event.deltaY < 0 ? step : -step;

    if (event.ctrlKey) {
      const newMin = Math.max(min, Math.min(maxValue - step, minValue + delta));
      minValue = newMin;
      dispatch('rangeChange', { min: minValue, max: maxValue });
      // Include staticValue to preserve it
      dispatch('sourceChange', { ...source, staticValue: staticValue, rangeMin: minValue, rangeMax: maxValue, midpoint: midpointEnabled });
    } else if (event.shiftKey) {
      const newMax = Math.max(minValue + step, Math.min(max, maxValue + delta));
      maxValue = newMax;
      dispatch('rangeChange', { min: minValue, max: maxValue });
      // Include staticValue to preserve it
      dispatch('sourceChange', { ...source, staticValue: staticValue, rangeMin: minValue, rangeMax: maxValue, midpoint: midpointEnabled });
    } else {
      const rangeSize = maxValue - minValue;
      const newMin = Math.max(min, Math.min(max - rangeSize, minValue + delta));
      const newMax = newMin + rangeSize;
      minValue = newMin;
      maxValue = newMax;
      dispatch('rangeChange', { min: minValue, max: maxValue });
      // Include staticValue to preserve it
      dispatch('sourceChange', { ...source, staticValue: staticValue, rangeMin: minValue, rangeMax: maxValue, midpoint: midpointEnabled });
    }
  }

  // Range area drag implementation
  let rangeTrackEl: HTMLDivElement;
  let draggingMode: 'min' | 'max' | 'range' | null = null;
  let dragStartX = 0;
  let dragStartMin = 0;
  let dragStartMax = 0;

  function handleRangeAreaMouseDown(event: MouseEvent) {
    const target = event.target as HTMLElement;
    if (target.classList.contains('range-thumb')) return;

    const rect = rangeTrackEl.getBoundingClientRect();
    const clickPercent = (event.clientX - rect.left) / rect.width;
    const clickValue = clickPercent * max;

    if (clickValue >= minValue && clickValue <= maxValue) {
      event.preventDefault();
      draggingMode = 'range';
      dragStartX = event.clientX;
      dragStartMin = minValue;
      dragStartMax = maxValue;
      document.addEventListener('mousemove', handleRangeMouseMove);
      document.addEventListener('mouseup', handleRangeMouseUp);
    }
  }

  function handleRangeMouseMove(event: MouseEvent) {
    if (!draggingMode || !rangeTrackEl) return;

    const rect = rangeTrackEl.getBoundingClientRect();
    const deltaX = event.clientX - dragStartX;
    const deltaValue = Math.round((deltaX / rect.width) * max / step) * step;

    if (draggingMode === 'range') {
      const rangeSize = dragStartMax - dragStartMin;
      let newMin = dragStartMin + deltaValue;
      let newMax = dragStartMax + deltaValue;

      if (newMin < min) {
        newMin = min;
        newMax = min + rangeSize;
      }
      if (newMax > max) {
        newMax = max;
        newMin = max - rangeSize;
      }

      minValue = newMin;
      maxValue = newMax;
      dispatch('rangeChange', { min: minValue, max: maxValue });
      // Include staticValue to preserve it
      dispatch('sourceChange', { ...source, staticValue: staticValue, rangeMin: minValue, rangeMax: maxValue, midpoint: midpointEnabled });
    }
  }

  function handleRangeMouseUp() {
    draggingMode = null;
    document.removeEventListener('mousemove', handleRangeMouseMove);
    document.removeEventListener('mouseup', handleRangeMouseUp);
  }

  onDestroy(() => {
    document.removeEventListener('mousemove', handleRangeMouseMove);
    document.removeEventListener('mouseup', handleRangeMouseUp);
  });
</script>

<div class="{showWrapper ? (compact ? 'space-y-1' : 'bg-card border rounded-lg p-4') : 'space-y-1'}">
  {#if showLabels}
  <!-- Label row with source indicator and value -->
  <div class="flex items-center justify-between text-xs">
    <div class="flex items-center gap-1.5">
      <!-- Parameter label (white) -->
      <span class="font-medium text-foreground">{parameterName}</span>

      <!-- Info tooltip -->
      {#if tooltip}
        <Tooltip content={tooltip}>
          <Info class="h-3 w-3 text-muted-foreground cursor-help" />
        </Tooltip>
      {/if}

      <!-- Source popover -->
      <Popover bind:open={popoverOpen} compact={true} contentClass="!w-[160px] min-w-0">
        <button
          slot="trigger"
          type="button"
          class="inline-flex items-center justify-center gap-0.5 px-1.5 h-5 rounded text-xs font-mono
                 bg-muted/50 hover:bg-muted border border-border/50 transition-colors min-w-[28px]
                 {(isLinked || hasButtplugLinks) ? (channel === 'A' ? 'text-primary' : 'text-secondary') : 'text-muted-foreground'}"
        >
          {#if inputMode === 'buttplug'}
            <!-- Buttplug mode: show linked features or default link icon -->
            {#if buttplugLinkItems.length > 0}
              {#each buttplugLinkItems as item}
                <svelte:component this={item.icon} class="h-3 w-3" />
                <span class="leading-none text-[10px]">{item.index}</span>
              {/each}
            {:else}
              <Link class="h-3.5 w-3.5 opacity-80" />
            {/if}
          {:else if inputMode === 'tcode' && isLinked}
            <!-- TCode mode with linked axis -->
            <span class="leading-none">{selectedSource}</span>
          {:else}
            <!-- No input or static mode -->
            <Link class="h-3.5 w-3.5 opacity-80" />
          {/if}
        </button>

        {#if inputMode === 'tcode' || inputMode === 'none'}
          <!-- T-Code Mode (or no input - default to tcode options) -->
          <div class="space-y-1 mb-2">
            {#each axisRows as row}
              <div class="flex gap-1">
                {#each row as axis}
                  <button
                    type="button"
                    class="flex-1 px-2 py-1 text-xs font-mono rounded transition-colors
                           {selectedSource === axis
                             ? (channel === 'A' ? 'bg-primary text-primary-foreground' : 'bg-secondary text-secondary-foreground')
                             : 'bg-muted/50 hover:bg-muted text-foreground'}"
                    on:click={() => handleAxisClick(axis)}
                  >
                    {axis}
                  </button>
                {/each}
              </div>
            {/each}
            <!-- Gamepad axis rows -->
            <div class="pt-1 border-t border-border/50 text-[10px] text-muted-foreground">Gamepad</div>
            {#each gamepadAxisRows as row}
              <div class="flex gap-1">
                {#each row as axis}
                  <button
                    type="button"
                    class="flex-1 px-2 py-1 text-[10px] font-mono rounded transition-colors
                           {selectedSource === axis
                             ? (channel === 'A' ? 'bg-primary text-primary-foreground' : 'bg-secondary text-secondary-foreground')
                             : 'bg-muted/50 hover:bg-muted text-foreground'}"
                    on:click={() => handleAxisClick(axis)}
                  >
                    {axis.replace('GP_', '')}
                  </button>
                {/each}
              </div>
            {/each}
          </div>

          <!-- Curve selector dropdown -->
          <select
            class="w-full px-2 py-1 pr-6 text-xs rounded border border-border bg-background text-foreground cursor-pointer appearance-none bg-no-repeat bg-right"
            style="background-image: url('data:image/svg+xml;charset=UTF-8,%3Csvg xmlns=%22http://www.w3.org/2000/svg%22 width=%2212%22 height=%2212%22 viewBox=%220 0 24 24%22 fill=%22none%22 stroke=%22%23888%22 stroke-width=%222%22%3E%3Cpath d=%22m6 9 6 6 6-6%22/%3E%3C/svg%3E'); background-position: right 6px center;"
            value={selectedCurve}
            on:change={handleCurveChange}
          >
            {#each curveOptions as option}
              <option value={option.value}>{option.label}</option>
            {/each}
          </select>

          <!-- Curve strength slider (only for exponential/logarithmic) -->
          {#if curveSupportsStrength}
          <div class="mt-2 space-y-1">
            <div class="flex justify-between items-center text-[10px] text-muted-foreground">
              <span>Strength</span>
              <span class="font-mono">{curveStrength.toFixed(1)}</span>
            </div>
            <Slider
              value={curveStrength}
              min={0.5}
              max={3.0}
              step={0.1}
              variant={channel === 'A' ? 'primary' : 'secondary'}
              on:change={handleStrengthChange}
              class="h-3"
            />
          </div>
          {/if}

          <!-- Midpoint toggle -->
          <label class="mt-2 flex items-center justify-between cursor-pointer">
            <span class="text-[10px] text-muted-foreground">Midpoint</span>
            <input
              type="checkbox"
              checked={midpointEnabled}
              on:change={handleMidpointChange}
              class="w-4 h-4 rounded border-border bg-background text-primary focus:ring-primary focus:ring-offset-0 cursor-pointer"
            />
          </label>
        {:else if inputMode === 'buttplug'}
          <!-- Buttplug Mode: Feature selection grid (TCode options hidden) -->
          <ButtplugLinkPanel
            {channel}
            source={source}
            on:linkChange={handleButtplugLinkChange}
          />
        {/if}
      </Popover>
    </div>

    <!-- Value display -->
    <div class="font-mono {channel === 'A' ? 'text-primary' : 'text-secondary'}">
      {#if isLinked}
        {#if isIntensity}
          {formatIntensityRange(minValue, maxValue)}
        {:else}
          {formatValue(minValue)} | {formatValue(maxValue)}
        {/if}
      {:else}
        {formatValue(staticValue)}
      {/if}
    </div>
  </div>
  {/if}

  {#if isLinked}
  <!-- Linked Mode: Range Slider with Position Indicator -->
  <!-- svelte-ignore a11y-no-static-element-interactions -->
  <div
    bind:this={rangeTrackEl}
    class="range-slider-container relative w-full h-6"
    style="--min-percent: {minPercent}%; --max-percent: {maxPercent}%; --slider-color: hsl(var(--{channel === 'A' ? 'primary' : 'secondary'})); --slider-shadow-1: hsl(var(--{channel === 'A' ? 'primary' : 'secondary'}) / 0.2); --slider-shadow-2: hsl(var(--{channel === 'A' ? 'primary' : 'secondary'}) / 0.6); --slider-shadow-3: hsl(var(--{channel === 'A' ? 'primary' : 'secondary'}) / 0.8)"
    on:wheel={handleRangeWheel}
    on:mousedown={handleRangeAreaMouseDown}
  >
    <!-- Track background -->
    <div class="absolute top-1/2 -translate-y-1/2 left-0 right-0 h-3 bg-muted rounded-full pointer-events-none"></div>

    <!-- Active range highlight (grabbable for dragging) -->
    <div
      class="absolute top-1/2 -translate-y-1/2 h-3 rounded-full {channel === 'A' ? 'bg-primary' : 'bg-secondary'} cursor-grab"
      class:cursor-grabbing={draggingMode === 'range'}
      style="left: calc(12px + {minPercent} * (100% - 24px) / 100); width: calc({maxPercent - minPercent} * (100% - 24px) / 100); box-shadow: 0 0 10px var(--slider-shadow-2)"
    ></div>

    <!-- Current input position indicator.
         Inset by half-thumb-width on each side so 0% / 100% align with
         where the thumb visually sits at min / max (browsers position the
         thumb's center at min+half-thumb on the left, max-half-thumb on
         the right, not at the absolute edges of the input). -->
    {#if indicatorValue > 0}
    <div
      class="position-indicator absolute top-1/2 pointer-events-none z-20"
      style="left: calc(12px + {indicatorPercent} * (100% - 24px) / 100);"
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
      {min}
      {max}
      {step}
      value={minValue}
      on:input={handleMinInput}
      class="range-input range-input-min range-thumb absolute top-1/2 -translate-y-1/2 w-full h-3 appearance-none bg-transparent cursor-pointer"
    />

    <!-- Max range input -->
    <input
      type="range"
      {min}
      {max}
      {step}
      value={maxValue}
      on:input={handleMaxInput}
      class="range-input range-input-max range-thumb absolute top-1/2 -translate-y-1/2 w-full h-3 appearance-none bg-transparent cursor-pointer"
    />
  </div>
  {:else}
  <!-- Static Mode: Single Value Slider -->
  <!-- svelte-ignore a11y-no-static-element-interactions -->
  <div
    class="relative w-full h-6"
    on:wheel={handleStaticWheel}
  >
    <!-- Track background -->
    <div class="absolute top-1/2 -translate-y-1/2 left-0 right-0 h-3 bg-muted rounded-full"></div>

    <!-- Value highlight: anchored to absolute left, fills full track width. -->
    <div
      class="absolute top-1/2 -translate-y-1/2 h-3 rounded-full {channel === 'A' ? 'bg-primary' : 'bg-secondary'}"
      style="left: 0; width: {(staticValue / max) * 100}%;"
    ></div>

    <!-- Static value input -->
    <input
      type="range"
      {min}
      {max}
      {step}
      value={staticValue}
      on:input={handleStaticInput}
      class="static-input absolute top-1/2 -translate-y-1/2 w-full h-3 appearance-none bg-transparent cursor-pointer"
      style="--slider-color: hsl(var(--{channel === 'A' ? 'primary' : 'secondary'})); --slider-shadow-1: hsl(var(--{channel === 'A' ? 'primary' : 'secondary'}) / 0.2); --slider-shadow-2: hsl(var(--{channel === 'A' ? 'primary' : 'secondary'}) / 0.6); --slider-shadow-3: hsl(var(--{channel === 'A' ? 'primary' : 'secondary'}) / 0.8)"
    />
  </div>
  {/if}
</div>

<style>
  .range-slider-container {
    touch-action: none;
  }

  .range-input,
  .static-input {
    pointer-events: none;
    margin: 0;
    padding: 0;
  }

  .range-input::-webkit-slider-thumb,
  .static-input::-webkit-slider-thumb {
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

  .range-input::-webkit-slider-thumb:hover,
  .static-input::-webkit-slider-thumb:hover {
    box-shadow: 0 0 0 6px var(--slider-shadow-1, hsl(var(--primary) / 0.2)), 0 0 25px var(--slider-shadow-3, hsl(var(--primary) / 0.8));
    transform: scale(1.1);
  }

  .range-input::-webkit-slider-thumb:active,
  .static-input::-webkit-slider-thumb:active {
    transform: scale(0.95);
  }

  .range-input::-moz-range-thumb,
  .static-input::-moz-range-thumb {
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

  .range-input::-moz-range-thumb:hover,
  .static-input::-moz-range-thumb:hover {
    box-shadow: 0 0 0 6px var(--slider-shadow-1, hsl(var(--primary) / 0.2)), 0 0 25px var(--slider-shadow-3, hsl(var(--primary) / 0.8));
    transform: scale(1.1);
  }

  .range-input::-moz-range-thumb:active,
  .static-input::-moz-range-thumb:active {
    transform: scale(0.95);
  }

  .range-input::-webkit-slider-runnable-track,
  .static-input::-webkit-slider-runnable-track {
    background: transparent;
  }

  .range-input::-moz-range-track,
  .static-input::-moz-range-track {
    background: transparent;
  }

  .range-input:focus,
  .static-input:focus {
    outline: none;
  }

  .range-input:focus-visible::-webkit-slider-thumb,
  .static-input:focus-visible::-webkit-slider-thumb {
    box-shadow: 0 0 0 3px hsl(var(--background)), 0 0 0 5px hsl(var(--ring));
  }

  .range-input:focus-visible::-moz-range-thumb,
  .static-input:focus-visible::-moz-range-thumb {
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
