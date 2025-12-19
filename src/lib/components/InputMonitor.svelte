<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { listen, type UnlistenFn } from '@tauri-apps/api/event';
  import { Activity, Radio, Zap, MapPin, RotateCw, MoveHorizontal, Minimize2 } from 'lucide-svelte';
  import { currentInputSource, updateButtplugFeatures } from '$lib/stores/inputSource';
  import { refreshConnectionStatus } from '$lib/stores/stateSync';
  import SynthWaveformChart from './SynthWaveformChart.svelte';
  import WaveformChart from './WaveformChart.svelte';

  interface ChannelOutput {
    raw_intensity: number;
    scaled_intensity: number;
    waveform: number[];
    frequency: number;
    range_min: number;
    range_max: number;
  }

  interface DeviceOutput {
    timestamp: number;
    channel_a: ChannelOutput;
    channel_b: ChannelOutput;
    is_connected: boolean;
  }

  // T-Code input state - dynamically populated from backend
  interface TCodeAxisValue {
    axis: string;
    value: number;
  }

  // Buttplug feature display state
  interface ButtplugFeatureDisplay {
    key: string;        // e.g., "Vibrate_0", "Linear_1"
    featureType: string; // e.g., "Vibrate", "Linear", "Rotate"
    index: number;       // Feature index (0, 1, 2...)
    value: number;       // 0.0-1.0
  }

  // Event payloads
  interface AxisUpdatePayload {
    axes: Record<string, number>;
    channel_a: number;
    channel_b: number;
    timestamp: number;
  }

  interface ButtplugFeaturesPayload {
    features: Record<string, number>;
    timestamp: number;
  }

  // Display state for T-Code axes - populated dynamically from received commands
  let tcodeAxes: TCodeAxisValue[] = [];

  // Buttplug features - populated dynamically from backend
  let buttplugFeatures: ButtplugFeatureDisplay[] = [];

  let isInputConnected = false;

  // Output state (Device)
  let deviceOutput: DeviceOutput | null = null;
  let isOutputConnected = false;

  // Waveform chart settings
  let waveformBufferMs = 2000;
  let chartType: 'synth' | 'envelope' = 'synth';

  // Event listeners
  let unlistenAxisUpdate: UnlistenFn | null = null;
  let unlistenButtplugFeatures: UnlistenFn | null = null;
  let statusPollInterval: ReturnType<typeof setInterval> | null = null;

  export let compact = true;

  // Subscribe to input source to detect changes
  $: inputSource = $currentInputSource;

  onMount(async () => {
    // Listen for T-Code axis updates (pushed from backend at 10Hz)
    unlistenAxisUpdate = await listen<AxisUpdatePayload>('axis-update', (event) => {
      const { axes } = event.payload;

      // Convert to array and sort by axis name
      const entries = Object.entries(axes) as [string, number][];
      tcodeAxes = entries
        .map(([axis, value]) => ({ axis, value }))
        .sort((a, b) => a.axis.localeCompare(b.axis));

      // Mark as connected when we receive axis data
      if (entries.length > 0) {
        isInputConnected = true;
      }
    });

    // Listen for Buttplug feature updates (pushed when commands received)
    unlistenButtplugFeatures = await listen<ButtplugFeaturesPayload>('buttplug-features', (event) => {
      const { features } = event.payload;

      // Convert to array with parsed feature info
      const entries = Object.entries(features) as [string, number][];
      buttplugFeatures = entries
        .map(([key, value]) => {
          const parts = key.split('_');
          const featureType = parts[0] || key;
          const index = parseInt(parts[1] || '0', 10);
          return { key, featureType, index, value };
        })
        .sort((a, b) => a.key.localeCompare(b.key));

      // Update the global store so other components can access these values
      updateButtplugFeatures(buttplugFeatures.map(f => ({
        featureType: f.featureType,
        featureIndex: f.index,
        value: f.value,
        label: `${f.featureType} ${f.index + 1}`
      })));

      // Mark as connected when we receive buttplug data
      if (entries.length > 0) {
        isInputConnected = true;
      }
    });

    // Poll less frequently for connection status, logs, and device output (1Hz)
    statusPollInterval = setInterval(pollStatus, 1000);

    // Initial poll
    pollStatus();
  });

  onDestroy(() => {
    if (unlistenAxisUpdate) unlistenAxisUpdate();
    if (unlistenButtplugFeatures) unlistenButtplugFeatures();
    if (statusPollInterval) clearInterval(statusPollInterval);
  });

  async function pollStatus() {
    try {
      // Refresh connection status
      const status = await refreshConnectionStatus();
      isInputConnected = status.websocket_running && status.detected_input_protocol !== 'none';

      // Get device output status
      const output = await invoke<DeviceOutput>('get_device_output');
      deviceOutput = output;
      isOutputConnected = output.is_connected;
    } catch (error) {
      console.error('Failed to poll status:', error);
    }
  }

  // Get progress bar percentage (value is 0-1)
  function getProgressPercent(value: number): number {
    return Math.min(100, Math.max(0, value * 100));
  }

  // Get icon component for Buttplug feature type
  function getFeatureIcon(featureType: string): any {
    switch (featureType) {
      case 'PositionWithDuration': return MapPin;
      case 'Rotate': return RotateCw;
      case 'Oscillate': return MoveHorizontal;
      case 'Vibrate': return Activity;
      case 'Constrict': return Minimize2;
      default: return Activity;
    }
  }
</script>

<div class="bg-card border border-border rounded-lg overflow-hidden {compact ? 'text-xs' : ''}">
  <!-- Header -->
  <div class="flex items-center justify-between px-3 py-2 bg-muted/30 border-b border-border">
    <div class="flex items-center gap-2">
      <Activity class="h-4 w-4 text-primary" />
      <span class="font-medium">Input Monitor</span>
    </div>
    <div class="flex items-center gap-3">
      <!-- Input status -->
      <span class="flex items-center gap-1 {isInputConnected ? 'text-green-500' : 'text-muted-foreground'}">
        <Radio class="h-3 w-3 {isInputConnected ? 'animate-pulse' : ''}" />
        <span class="text-xs">In</span>
      </span>
      <!-- Output status -->
      <span class="flex items-center gap-1 {isOutputConnected ? 'text-green-500' : 'text-muted-foreground'}">
        <Zap class="h-3 w-3 {isOutputConnected ? 'animate-pulse' : ''}" />
        <span class="text-xs">Out</span>
      </span>
    </div>
  </div>

  <!-- Input & Output Side by Side -->
  <div class="grid grid-cols-2 divide-x divide-border">
    <!-- INPUT Section -->
    <div class="p-3 flex flex-col">
      <div class="text-[10px] text-muted-foreground mb-2 font-medium">INPUT (Target)</div>

      {#if inputSource === 'tcode'}
        <!-- T-Code Mode: Show axis values in 2 columns -->
        {#if tcodeAxes.length > 0}
          <div class="grid grid-cols-2 gap-1 font-mono text-[10px]">
            {#each tcodeAxes as axis}
              <div class="relative h-4 bg-muted rounded-sm overflow-hidden">
                <div
                  class="absolute inset-y-0 left-0 bg-primary/50 transition-all duration-75"
                  style="width: {getProgressPercent(axis.value)}%"
                />
                <div class="absolute inset-0 flex items-center justify-between px-1.5">
                  <span class="font-medium text-foreground">{axis.axis}</span>
                  <span class="text-foreground/80">{Math.round(axis.value * 100)}</span>
                </div>
              </div>
            {/each}
          </div>
        {:else}
          <div class="flex-1 flex items-center justify-center text-muted-foreground text-[10px]">
            Waiting for T-Code...
          </div>
        {/if}
      {:else if inputSource === 'buttplug'}
        <!-- Buttplug Mode: Show feature values with icons -->
        {#if buttplugFeatures.length > 0}
          <div class="grid grid-cols-2 gap-1 font-mono text-[10px]">
            {#each buttplugFeatures as feature}
              <div class="relative h-4 bg-muted rounded-sm overflow-hidden">
                <div
                  class="absolute inset-y-0 left-0 bg-primary/50 transition-all duration-75"
                  style="width: {getProgressPercent(feature.value)}%"
                />
                <div class="absolute inset-0 flex items-center justify-between px-1.5">
                  <span class="flex items-center gap-0.5 font-medium text-foreground">
                    <svelte:component this={getFeatureIcon(feature.featureType)} class="h-2.5 w-2.5" />
                    <span class="text-[9px]">{feature.index + 1}</span>
                  </span>
                  <span class="text-foreground/80">{Math.round(feature.value * 100)}</span>
                </div>
              </div>
            {/each}
          </div>
        {:else}
          <div class="flex-1 flex items-center justify-center text-muted-foreground text-[10px]">
            Waiting for Buttplug...
          </div>
        {/if}
      {:else}
        <!-- No input connected -->
        <div class="flex-1 flex items-center justify-center text-muted-foreground text-[10px]">
          Waiting for input connection...
        </div>
      {/if}
    </div>

    <!-- OUTPUT Section - Waveform Chart -->
    <div class="p-3">
      <div class="flex items-center justify-between mb-2">
        <span class="text-[10px] text-muted-foreground font-medium">OUTPUT (Device)</span>
        <div class="flex items-center gap-2">
          <select
            bind:value={chartType}
            class="text-[10px] bg-muted text-muted-foreground border-none rounded px-1 py-0.5 cursor-pointer"
            title="Chart display type"
          >
            <option value="synth">Synth</option>
            <option value="envelope">Envelope</option>
          </select>
          <input
            type="range"
            min="1000"
            max="10000"
            step="1000"
            bind:value={waveformBufferMs}
            class="buffer-slider w-16 h-2 cursor-pointer rounded-full"
            style="background: linear-gradient(to right, hsl(var(--primary)) 0%, hsl(var(--primary)) {((waveformBufferMs - 1000) / 9000) * 100}%, hsl(var(--muted)) {((waveformBufferMs - 1000) / 9000) * 100}%, hsl(var(--muted)) 100%)"
            title="Buffer duration: {waveformBufferMs / 1000}s"
          />
          <span class="text-[10px] text-muted-foreground w-4">{waveformBufferMs / 1000}s</span>
        </div>
      </div>
      {#if chartType === 'synth'}
        <SynthWaveformChart height={100} bufferDurationMs={waveformBufferMs} />
      {:else}
        <WaveformChart height={100} bufferDurationMs={waveformBufferMs} />
      {/if}
    </div>
  </div>
</div>

<style>
  /* Compact slider for buffer duration - similar to main Slider but smaller */
  .buffer-slider {
    -webkit-appearance: none;
    appearance: none;
  }

  .buffer-slider::-webkit-slider-thumb {
    -webkit-appearance: none;
    appearance: none;
    width: 14px;
    height: 14px;
    background: hsl(var(--primary));
    border: 2px solid hsl(var(--background));
    border-radius: 50%;
    cursor: pointer;
    box-shadow: 0 0 0 1px hsl(var(--primary) / 0.2), 0 0 8px hsl(var(--primary) / 0.4);
    transition: all 0.15s ease;
  }

  .buffer-slider::-webkit-slider-thumb:hover {
    box-shadow: 0 0 0 3px hsl(var(--primary) / 0.2), 0 0 12px hsl(var(--primary) / 0.6);
    transform: scale(1.1);
  }

  .buffer-slider::-moz-range-thumb {
    width: 14px;
    height: 14px;
    background: hsl(var(--primary));
    border: 2px solid hsl(var(--background));
    border-radius: 50%;
    cursor: pointer;
    box-shadow: 0 0 0 1px hsl(var(--primary) / 0.2), 0 0 8px hsl(var(--primary) / 0.4);
    transition: all 0.15s ease;
  }

  .buffer-slider::-moz-range-thumb:hover {
    box-shadow: 0 0 0 3px hsl(var(--primary) / 0.2), 0 0 12px hsl(var(--primary) / 0.6);
    transform: scale(1.1);
  }
</style>
