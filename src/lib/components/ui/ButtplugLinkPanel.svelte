<script lang="ts">
  import { createEventDispatcher } from 'svelte';
  import type { ButtplugFeatureLink, ButtplugFeatureType, ButtplugFeatureConfig, ParameterSource } from '$lib/types/modulation.js';
  import { MapPin, RotateCw, MoveHorizontal, Activity, Minimize2 } from 'lucide-svelte';
  import Slider from './Slider.svelte';
  import Tooltip from './Tooltip.svelte';

  // Props
  export let channel: 'A' | 'B';
  export let source: ParameterSource;
  export let featureCounts: {
    position: number;
    positionWithDuration: number;
    vibrate: number;
    rotate: number;
    oscillate: number;
    constrict: number;
  } = {
    position: 0,  // Not used - clients prefer LinearCmd (PositionWithDuration)
    positionWithDuration: 2,
    vibrate: 2,
    rotate: 2,
    oscillate: 2,
    constrict: 2
  };

  const dispatch = createEventDispatcher<{
    linkChange: ParameterSource;
  }>();

  // Feature type icons
  const icons: Record<string, any> = {
    PositionWithDuration: MapPin,
    Rotate: RotateCw,
    Oscillate: MoveHorizontal,
    Vibrate: Activity,
    Constrict: Minimize2
  };

  // Local state for immediate UI feedback
  // These are updated immediately on click AND synced from props
  let selectedPosition: ButtplugFeatureLink | null = source.buttplugLinks?.position ?? null;
  let selectedMotion: ButtplugFeatureLink | null = source.buttplugLinks?.motion ?? null;
  let selectedVibrate: ButtplugFeatureLink | null = source.buttplugLinks?.vibrate ?? null;
  let selectedConstrict: ButtplugFeatureLink | null = source.buttplugLinks?.constrict ?? null;

  // Track whether we're in the middle of a local update (to avoid prop sync overwriting)
  let isLocalUpdate = false;

  // Sync local state with prop changes (when parent updates source)
  // Skip sync if we just made a local update (let the DOM update first)
  $: {
    if (!isLocalUpdate) {
      const propPosition = source.buttplugLinks?.position ?? null;
      const propMotion = source.buttplugLinks?.motion ?? null;
      const propVibrate = source.buttplugLinks?.vibrate ?? null;
      const propConstrict = source.buttplugLinks?.constrict ?? null;

      // Only update if actually different (compare by identity for objects)
      if (propPosition !== selectedPosition) selectedPosition = propPosition;
      if (propMotion !== selectedMotion) selectedMotion = propMotion;
      if (propVibrate !== selectedVibrate) selectedVibrate = propVibrate;
      if (propConstrict !== selectedConstrict) selectedConstrict = propConstrict;
    }
  }

  // Reactive selection checks - these ARE used in the template for reactive updates
  // By referencing these in the template, Svelte knows to re-render when they change
  $: isPositionSelected = (type: ButtplugFeatureType, index: number) =>
    selectedPosition?.featureType === type && selectedPosition?.featureIndex === index;
  $: isMotionSelected = (type: ButtplugFeatureType, index: number) =>
    selectedMotion?.featureType === type && selectedMotion?.featureIndex === index;
  $: isVibrateSelected = (type: ButtplugFeatureType, index: number) =>
    selectedVibrate?.featureType === type && selectedVibrate?.featureIndex === index;
  $: isConstrictSelected = (type: ButtplugFeatureType, index: number) =>
    selectedConstrict?.featureType === type && selectedConstrict?.featureIndex === index;

  // Check if a specific feature is selected (uses reactive functions for proper updates)
  function isSelected(stage: 'position' | 'motion' | 'vibrate' | 'constrict', type: ButtplugFeatureType, index: number): boolean {
    if (stage === 'position') return isPositionSelected(type, index);
    if (stage === 'motion') return isMotionSelected(type, index);
    if (stage === 'vibrate') return isVibrateSelected(type, index);
    if (stage === 'constrict') return isConstrictSelected(type, index);
    return false;
  }

  // Handle feature button click
  function handleFeatureClick(stage: 'position' | 'motion' | 'vibrate' | 'constrict', type: ButtplugFeatureType, index: number) {
    const alreadySelected = isSelected(stage, type, index);

    let newPosition = selectedPosition;
    let newMotion = selectedMotion;
    let newVibrate = selectedVibrate;
    let newConstrict = selectedConstrict;

    if (alreadySelected) {
      // Deselect
      if (stage === 'position') newPosition = null;
      else if (stage === 'motion') newMotion = null;
      else if (stage === 'vibrate') newVibrate = null;
      else if (stage === 'constrict') newConstrict = null;
    } else {
      // Select (deselecting others in same stage)
      const newLink: ButtplugFeatureLink = {
        featureType: type,
        featureIndex: index,
        config: getDefaultConfig(type)
      };

      if (stage === 'position') newPosition = newLink;
      else if (stage === 'motion') newMotion = newLink;
      else if (stage === 'vibrate') newVibrate = newLink;
      else if (stage === 'constrict') newConstrict = newLink;
    }

    // Mark as local update to prevent reactive sync from overwriting
    isLocalUpdate = true;

    // Update local state IMMEDIATELY for instant UI feedback
    selectedPosition = newPosition;
    selectedMotion = newMotion;
    selectedVibrate = newVibrate;
    selectedConstrict = newConstrict;

    // Dispatch change to parent (will also update source prop which syncs back)
    emitChange(newPosition, newMotion, newVibrate, newConstrict);

    // Reset flag after a tick to allow future prop syncs
    setTimeout(() => { isLocalUpdate = false; }, 0);
  }

  // Get default config for a feature type
  function getDefaultConfig(type: ButtplugFeatureType): ButtplugFeatureConfig {
    switch (type) {
      case 'Vibrate':
        return { distance: 0.2 };
      case 'Rotate':
        return { rotateScale: 0.5, rotateMaxSpeed: 5.0 };
      case 'Oscillate':
        return { oscillateScale: 0.5, oscillateMaxSpeed: 5.0 };
      case 'Constrict':
        return { constrictMinFloor: 0.0, constrictUseMidpoint: false, constrictMethod: 'downsample' };
      default:
        return {};
    }
  }

  // Handle config changes
  function handleConfigChange(stage: 'position' | 'motion' | 'vibrate' | 'constrict', field: keyof ButtplugFeatureConfig, value: number | boolean | string) {
    // Use local state for current values
    const target = stage === 'position' ? selectedPosition :
                   stage === 'motion' ? selectedMotion :
                   stage === 'vibrate' ? selectedVibrate :
                   selectedConstrict;

    if (!target) return;

    // Create updated link with new config
    const updatedLink: ButtplugFeatureLink = {
      ...target,
      config: { ...target.config, [field]: value }
    };

    // Mark as local update to prevent reactive sync from overwriting
    isLocalUpdate = true;

    // Update local state immediately
    if (stage === 'position') selectedPosition = updatedLink;
    else if (stage === 'motion') selectedMotion = updatedLink;
    else if (stage === 'vibrate') selectedVibrate = updatedLink;
    else if (stage === 'constrict') selectedConstrict = updatedLink;

    // Emit with updated config
    const newPosition = stage === 'position' ? updatedLink : selectedPosition;
    const newMotion = stage === 'motion' ? updatedLink : selectedMotion;
    const newVibrate = stage === 'vibrate' ? updatedLink : selectedVibrate;
    const newConstrict = stage === 'constrict' ? updatedLink : selectedConstrict;

    emitChange(newPosition, newMotion, newVibrate, newConstrict);

    // Reset flag after a tick to allow future prop syncs
    setTimeout(() => { isLocalUpdate = false; }, 0);
  }

  // Emit change event
  function emitChange(
    position: ButtplugFeatureLink | null,
    motion: ButtplugFeatureLink | null,
    vibrate: ButtplugFeatureLink | null,
    constrict: ButtplugFeatureLink | null
  ) {
    const newSource: ParameterSource = {
      ...source,
      buttplugLinks: {
        position: position ?? undefined,
        motion: motion ?? undefined,
        vibrate: vibrate ?? undefined,
        constrict: constrict ?? undefined
      }
    };

    dispatch('linkChange', newSource);
  }

  // Has Position feature selected (for Constrict midpoint toggle visibility) - uses local state
  $: hasPositionSelected = selectedPosition !== null;
</script>

<div class="space-y-2">
  <!-- Feature buttons grouped by type in rows -->
  <div class="space-y-1">
    <!-- Position row (Position + PositionWithDuration) -->
    {#if featureCounts.position > 0 || featureCounts.positionWithDuration > 0}
    <div class="grid grid-cols-4 gap-1">
      <!-- Position features -->
      {#each Array.from({ length: featureCounts.position }) as _, i}
        <Tooltip content="Position {i + 1}">
          <button
            type="button"
            class="flex items-center justify-center gap-0.5 px-1.5 py-1 text-xs font-mono rounded transition-colors
                   {isPositionSelected('Position', i)
                     ? (channel === 'A' ? 'bg-primary text-primary-foreground' : 'bg-secondary text-secondary-foreground')
                     : 'bg-muted/50 hover:bg-muted text-foreground'}"
            on:click={() => handleFeatureClick('position', 'Position', i)}
          >
            <svelte:component this={icons.Position} class="h-3 w-3" />
            <span>{i + 1}</span>
          </button>
        </Tooltip>
      {/each}
      <!-- Position with Duration features -->
      {#each Array.from({ length: featureCounts.positionWithDuration }) as _, i}
        <Tooltip content="Linear {i + 1} (Position + Duration)">
          <button
            type="button"
            class="flex items-center justify-center gap-0.5 px-1.5 py-1 text-xs font-mono rounded transition-colors
                   {isPositionSelected('PositionWithDuration', i)
                     ? (channel === 'A' ? 'bg-primary text-primary-foreground' : 'bg-secondary text-secondary-foreground')
                     : 'bg-muted/50 hover:bg-muted text-foreground'}"
            on:click={() => handleFeatureClick('position', 'PositionWithDuration', i)}
          >
            <svelte:component this={icons.PositionWithDuration} class="h-3 w-3" />
            <span>{i + 1}</span>
          </button>
        </Tooltip>
      {/each}
    </div>
    {/if}

    <!-- Motion row (Rotate + Oscillate) -->
    {#if featureCounts.rotate > 0 || featureCounts.oscillate > 0}
    <div class="grid grid-cols-4 gap-1">
      <!-- Rotate features -->
      {#each Array.from({ length: featureCounts.rotate }) as _, i}
        <Tooltip content="Rotate {i + 1}">
          <button
            type="button"
            class="flex items-center justify-center gap-0.5 px-1.5 py-1 text-xs font-mono rounded transition-colors
                   {isMotionSelected('Rotate', i)
                     ? (channel === 'A' ? 'bg-primary text-primary-foreground' : 'bg-secondary text-secondary-foreground')
                     : 'bg-muted/50 hover:bg-muted text-foreground'}"
            on:click={() => handleFeatureClick('motion', 'Rotate', i)}
          >
            <svelte:component this={icons.Rotate} class="h-3 w-3" />
            <span>{i + 1}</span>
          </button>
        </Tooltip>
      {/each}
      <!-- Oscillate features -->
      {#each Array.from({ length: featureCounts.oscillate }) as _, i}
        <Tooltip content="Oscillate {i + 1}">
          <button
            type="button"
            class="flex items-center justify-center gap-0.5 px-1.5 py-1 text-xs font-mono rounded transition-colors
                   {isMotionSelected('Oscillate', i)
                     ? (channel === 'A' ? 'bg-primary text-primary-foreground' : 'bg-secondary text-secondary-foreground')
                     : 'bg-muted/50 hover:bg-muted text-foreground'}"
            on:click={() => handleFeatureClick('motion', 'Oscillate', i)}
          >
            <svelte:component this={icons.Oscillate} class="h-3 w-3" />
            <span>{i + 1}</span>
          </button>
        </Tooltip>
      {/each}
    </div>
    {/if}

    <!-- Vibrate row -->
    {#if featureCounts.vibrate > 0}
    <div class="grid grid-cols-4 gap-1">
      <!-- Vibrate features -->
      {#each Array.from({ length: featureCounts.vibrate }) as _, i}
        <Tooltip content="Vibrate {i + 1}">
          <button
            type="button"
            class="flex items-center justify-center gap-0.5 px-1.5 py-1 text-xs font-mono rounded transition-colors
                   {isVibrateSelected('Vibrate', i)
                     ? (channel === 'A' ? 'bg-primary text-primary-foreground' : 'bg-secondary text-secondary-foreground')
                     : 'bg-muted/50 hover:bg-muted text-foreground'}"
            on:click={() => handleFeatureClick('vibrate', 'Vibrate', i)}
          >
            <svelte:component this={icons.Vibrate} class="h-3 w-3" />
            <span>{i + 1}</span>
          </button>
        </Tooltip>
      {/each}
    </div>
    {/if}

    <!-- Constrict row -->
    {#if featureCounts.constrict > 0}
    <div class="grid grid-cols-4 gap-1">
      <!-- Constrict features -->
      {#each Array.from({ length: featureCounts.constrict }) as _, i}
        <Tooltip content="Constrict {i + 1}">
          <button
            type="button"
            class="flex items-center justify-center gap-0.5 px-1.5 py-1 text-xs font-mono rounded transition-colors
                   {isConstrictSelected('Constrict', i)
                     ? (channel === 'A' ? 'bg-primary text-primary-foreground' : 'bg-secondary text-secondary-foreground')
                     : 'bg-muted/50 hover:bg-muted text-foreground'}"
            on:click={() => handleFeatureClick('constrict', 'Constrict', i)}
          >
            <svelte:component this={icons.Constrict} class="h-3 w-3" />
            <span>{i + 1}</span>
          </button>
        </Tooltip>
      {/each}
    </div>
    {/if}
  </div>

  <!-- Feature Config Section -->
  {#if selectedVibrate || selectedMotion || selectedConstrict}
    <div class="pt-2 space-y-3">
      <!-- Vibrate Config -->
      {#if selectedVibrate}
        <div class="space-y-2">
          <div class="flex justify-between items-center text-[10px]">
            <span class="text-muted-foreground">
              <svelte:component this={icons.Vibrate} class="inline h-3 w-3 mr-0.5" />
              Vibrate {selectedVibrate.featureIndex + 1}
            </span>
          </div>
          <div class="space-y-1">
            <div class="flex justify-between items-center text-[10px] text-muted-foreground">
              <span>Distance</span>
              <span class="font-mono">{(selectedVibrate.config?.distance ?? 0.2).toFixed(2)}</span>
            </div>
            <Slider
              value={selectedVibrate.config?.distance ?? 0.2}
              min={0.0}
              max={1.0}
              step={0.05}
              variant={channel === 'A' ? 'primary' : 'secondary'}
              on:change={(e) => handleConfigChange('vibrate', 'distance', e.detail)}
              class="h-3"
            />
          </div>
        </div>
      {/if}

      <!-- Motion Config (Rotate or Oscillate) -->
      {#if selectedMotion}
        <div class="space-y-2">
          <div class="flex justify-between items-center text-[10px]">
            <span class="text-muted-foreground">
              <svelte:component this={icons[selectedMotion.featureType]} class="inline h-3 w-3 mr-0.5" />
              {selectedMotion.featureType} {selectedMotion.featureIndex + 1}
            </span>
          </div>
          <div class="space-y-1">
            <div class="flex justify-between items-center text-[10px] text-muted-foreground">
              <span>Scale</span>
              <span class="font-mono">
                {selectedMotion?.featureType === 'Rotate'
                  ? (selectedMotion?.config?.rotateScale ?? 0.5).toFixed(2)
                  : (selectedMotion?.config?.oscillateScale ?? 0.5).toFixed(2)}
              </span>
            </div>
            <Slider
              value={selectedMotion?.featureType === 'Rotate'
                ? (selectedMotion?.config?.rotateScale ?? 0.5)
                : (selectedMotion?.config?.oscillateScale ?? 0.5)}
              min={0.0}
              max={1.0}
              step={0.05}
              variant={channel === 'A' ? 'primary' : 'secondary'}
              on:change={(e) => handleConfigChange('motion',
                selectedMotion?.featureType === 'Rotate' ? 'rotateScale' : 'oscillateScale',
                e.detail)}
              class="h-3"
            />
          </div>
          <div class="space-y-1">
            <div class="flex justify-between items-center text-[10px] text-muted-foreground">
              <span>Max Speed (Hz)</span>
              <span class="font-mono">
                {selectedMotion?.featureType === 'Rotate'
                  ? (selectedMotion?.config?.rotateMaxSpeed ?? 5.0).toFixed(1)
                  : (selectedMotion?.config?.oscillateMaxSpeed ?? 5.0).toFixed(1)}
              </span>
            </div>
            <Slider
              value={selectedMotion?.featureType === 'Rotate'
                ? (selectedMotion?.config?.rotateMaxSpeed ?? 5.0)
                : (selectedMotion?.config?.oscillateMaxSpeed ?? 5.0)}
              min={0.5}
              max={10.0}
              step={0.5}
              variant={channel === 'A' ? 'primary' : 'secondary'}
              on:change={(e) => handleConfigChange('motion',
                selectedMotion?.featureType === 'Rotate' ? 'rotateMaxSpeed' : 'oscillateMaxSpeed',
                e.detail)}
              class="h-3"
            />
          </div>
        </div>
      {/if}

      <!-- Constrict Config -->
      {#if selectedConstrict}
        <div class="space-y-2">
          <div class="flex justify-between items-center text-[10px]">
            <span class="text-muted-foreground">
              <svelte:component this={icons.Constrict} class="inline h-3 w-3 mr-0.5" />
              Constrict {selectedConstrict.featureIndex + 1}
            </span>
          </div>
          <div class="space-y-1">
            <div class="flex justify-between items-center text-[10px] text-muted-foreground">
              <span>Min Floor</span>
              <span class="font-mono">{(selectedConstrict.config?.constrictMinFloor ?? 0.0).toFixed(2)}</span>
            </div>
            <Slider
              value={selectedConstrict.config?.constrictMinFloor ?? 0.0}
              min={0.0}
              max={1.0}
              step={0.05}
              variant={channel === 'A' ? 'primary' : 'secondary'}
              on:change={(e) => handleConfigChange('constrict', 'constrictMinFloor', e.detail)}
              class="h-3"
            />
          </div>
          <div class="space-y-1">
            <label class="flex items-center justify-between text-xs cursor-pointer">
              <span class="text-muted-foreground">Method</span>
              <select
                class="px-2 py-0.5 text-xs rounded border border-border bg-background text-foreground cursor-pointer"
                value={selectedConstrict.config?.constrictMethod ?? 'downsample'}
                on:change={(e) => handleConfigChange('constrict', 'constrictMethod', e.currentTarget.value)}
              >
                <option value="downsample">Downsample</option>
                <option value="clamp">Clamp</option>
              </select>
            </label>
          </div>
          {#if hasPositionSelected}
            <div class="space-y-1">
              <label class="flex items-center justify-between cursor-pointer">
                <span class="text-[10px] text-muted-foreground">Use Midpoint</span>
                <input
                  type="checkbox"
                  checked={selectedConstrict.config?.constrictUseMidpoint ?? false}
                  on:change={(e) => handleConfigChange('constrict', 'constrictUseMidpoint', e.currentTarget.checked)}
                  class="w-4 h-4 rounded border-border bg-background text-primary focus:ring-primary focus:ring-offset-0 cursor-pointer"
                />
              </label>
            </div>
          {/if}
        </div>
      {/if}
    </div>
  {/if}
</div>
