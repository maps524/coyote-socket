<script lang="ts">
  /**
   * Example usage of RangeSliderWithIndicator component
   * This file demonstrates both static and linked modes with all features
   */
  import RangeSliderWithIndicator from './RangeSliderWithIndicator.svelte';
  import type { ParameterSource } from '$lib/types/modulation.js';
  import { inputPositionA, inputPositionB } from '$lib/stores/inputPosition.js';
  import { onMount, onDestroy } from 'svelte';

  // Example 1: Frequency control (starts in static mode)
  let frequencySource: ParameterSource = {
    type: 'static',
    staticValue: 100,
    rangeMin: 1,
    rangeMax: 200,
    curve: 'linear'
  };

  // Example 2: Intensity control (starts in linked mode with L0)
  let intensitySource: ParameterSource = {
    type: 'linked',
    sourceAxis: 'L0',
    rangeMin: 10,
    rangeMax: 150,
    curve: 'linear',
    curveStrength: 2.0
  };

  // Example 3: Balance control with exponential curve
  let balanceSource: ParameterSource = {
    type: 'linked',
    sourceAxis: 'R2',
    rangeMin: 0,
    rangeMax: 255,
    curve: 'exponential',
    curveStrength: 2.0
  };

  // Simulated input position (animates 0-1 for demonstration)
  let simulatedInput = 0;
  let animationId: number | null = null;

  onMount(() => {
    // Animate the simulated input for demo purposes
    function animate() {
      simulatedInput = (Math.sin(Date.now() / 1000) + 1) / 2; // 0-1 sine wave
      animationId = requestAnimationFrame(animate);
    }
    animate();
  });

  onDestroy(() => {
    if (animationId !== null) {
      cancelAnimationFrame(animationId);
    }
  });

  // Event handlers
  function handleFrequencyChange(event: CustomEvent<ParameterSource>) {
    frequencySource = event.detail;
    console.log('Frequency source changed:', frequencySource);
  }

  function handleIntensityChange(event: CustomEvent<ParameterSource>) {
    intensitySource = event.detail;
    console.log('Intensity source changed:', intensitySource);
  }

  function handleBalanceChange(event: CustomEvent<ParameterSource>) {
    balanceSource = event.detail;
    console.log('Balance source changed:', balanceSource);
  }

  function handleRangeChange(name: string, event: CustomEvent<{ min: number; max: number }>) {
    console.log(`${name} range changed:`, event.detail);
  }
</script>

<div class="p-6 space-y-8 bg-background text-foreground min-h-screen">
  <div class="max-w-2xl mx-auto space-y-6">
    <h1 class="text-2xl font-bold">RangeSliderWithIndicator Examples</h1>

    <!-- Example 1: Static Mode -->
    <div class="space-y-2">
      <h2 class="text-lg font-semibold text-primary">Example 1: Static Mode</h2>
      <p class="text-sm text-muted-foreground">
        Manual control with a single value. No position indicator visible.
      </p>
      <RangeSliderWithIndicator
        channel="A"
        parameterName="Frequency (Hz)"
        source={frequencySource}
        min={1}
        max={200}
        step={1}
        indicatorValue={$inputPositionA}
        on:sourceChange={handleFrequencyChange}
        on:rangeChange={(e) => handleRangeChange('Frequency', e)}
      />
      <div class="text-xs text-muted-foreground font-mono bg-muted p-2 rounded">
        Current: {JSON.stringify(frequencySource, null, 2)}
      </div>
    </div>

    <!-- Example 2: Linked Mode with Real Input Position -->
    <div class="space-y-2">
      <h2 class="text-lg font-semibold text-primary">Example 2: Linked Mode (Real Data)</h2>
      <p class="text-sm text-muted-foreground">
        Linked to L0 with position indicator showing current T-Code input (Channel A).
      </p>
      <RangeSliderWithIndicator
        channel="A"
        parameterName="Intensity"
        source={intensitySource}
        min={0}
        max={200}
        step={2}
        indicatorValue={$inputPositionA}
        on:sourceChange={handleIntensityChange}
        on:rangeChange={(e) => handleRangeChange('Intensity', e)}
      />
      <div class="text-xs text-muted-foreground font-mono bg-muted p-2 rounded">
        Position A: {($inputPositionA * 100).toFixed(1)}%
        <br />
        Current: {JSON.stringify(intensitySource, null, 2)}
      </div>
    </div>

    <!-- Example 3: Linked Mode with Simulated Input -->
    <div class="space-y-2">
      <h2 class="text-lg font-semibold text-secondary">Example 3: Linked Mode (Simulated)</h2>
      <p class="text-sm text-muted-foreground">
        Linked to R2 with exponential curve. Position animates for demonstration.
      </p>
      <RangeSliderWithIndicator
        channel="B"
        parameterName="Balance"
        source={balanceSource}
        min={0}
        max={255}
        step={5}
        indicatorValue={simulatedInput}
        on:sourceChange={handleBalanceChange}
        on:rangeChange={(e) => handleRangeChange('Balance', e)}
      />
      <div class="text-xs text-muted-foreground font-mono bg-muted p-2 rounded">
        Simulated Input: {(simulatedInput * 100).toFixed(1)}%
        <br />
        Current: {JSON.stringify(balanceSource, null, 2)}
      </div>
    </div>

    <!-- Interaction Help -->
    <div class="border border-border rounded-lg p-4 bg-card">
      <h3 class="text-sm font-semibold mb-2">Interaction Guide</h3>
      <ul class="text-xs space-y-1 text-muted-foreground">
        <li>• <strong>Source Dropdown:</strong> Switch between Static and T-Code axes (L0, R2, etc.)</li>
        <li>• <strong>Curve Dropdown:</strong> Select transformation curve (only in linked mode)</li>
        <li>• <strong>Drag Handles:</strong> Adjust min/max range (linked) or value (static)</li>
        <li>• <strong>Mouse Wheel:</strong> Scroll to adjust range</li>
        <li>• <strong>Ctrl + Wheel:</strong> Adjust minimum only</li>
        <li>• <strong>Shift + Wheel:</strong> Adjust maximum only</li>
        <li>• <strong>Position Indicator:</strong> White line shows current T-Code input (linked mode only)</li>
      </ul>
    </div>

    <!-- Debug Panel -->
    <div class="border border-border rounded-lg p-4 bg-card">
      <h3 class="text-sm font-semibold mb-2">Debug Info</h3>
      <div class="text-xs space-y-1 text-muted-foreground font-mono">
        <div>Input Position A: {($inputPositionA * 100).toFixed(2)}%</div>
        <div>Input Position B: {($inputPositionB * 100).toFixed(2)}%</div>
        <div>Simulated Input: {(simulatedInput * 100).toFixed(2)}%</div>
      </div>
    </div>
  </div>
</div>
