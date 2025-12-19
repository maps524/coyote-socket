<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { listen, type UnlistenFn } from '@tauri-apps/api/event';

  // Props
  export let height = 120;
  export let bufferDurationMs = 1000; // How much history to show (1-10 seconds)

  // Types matching backend WaveformSample
  interface WaveformSample {
    timestamp: number;
    channel_a_intensity: number;      // 0-200 (scaled)
    channel_b_intensity: number;      // 0-200 (scaled)
    channel_a_frequency: number;      // Hz
    channel_b_frequency: number;      // Hz
    channel_a_freq_balance: number;   // 0-255 (duty cycle)
    channel_b_freq_balance: number;   // 0-255 (duty cycle)
    channel_a_int_balance: number;    // 0-255 (vertical asymmetry)
    channel_b_int_balance: number;    // 0-255 (vertical asymmetry)
    channel_a_waveform: number[];     // [4] 0-100
    channel_b_waveform: number[];     // [4] 0-100
  }

  interface WaveformParams {
    intensity: number;      // 0-200
    frequency: number;      // Hz
    freqBalance: number;    // 0-255 (duty cycle)
    intBalance: number;     // 0-255 (vertical asymmetry)
  }

  interface CycleGeometry {
    cyclesPerSlice: number;
    dutyRatio: number;      // 0-1
    topY: number;
    bottomY: number;
    centerY: number;
    amplitude: number;
  }

  // State
  let canvas: HTMLCanvasElement;
  let ctx: CanvasRenderingContext2D | null = null;
  let animationFrame: number | null = null;
  let unlistenWaveform: UnlistenFn | null = null;

  let samples: WaveformSample[] = [];

  // Constants
  const MIN_PIXELS_PER_CYCLE = 4;       // Below this, render simplified
  const MIN_SAMPLES_PER_CYCLE = 8;
  const MAX_SAMPLES_PER_CYCLE = 48;

  // Theme colors (matching CSS variables)
  const channelAColor = 'hsl(280, 70%, 60%)';      // Channel A - purple
  const channelBColor = 'hsl(200, 80%, 55%)';      // Channel B - blue
  const channelAColorAlpha = 'hsla(280, 70%, 60%, 0.4)';
  const channelBColorAlpha = 'hsla(200, 80%, 55%, 0.4)';
  const gridColor = 'hsl(240, 10%, 20%)';
  const bgColor = 'hsl(240, 10%, 8%)';

  onMount(async () => {
    ctx = canvas.getContext('2d');

    // Listen for waveform samples pushed from backend
    unlistenWaveform = await listen<WaveformSample>('waveform-sample', (event) => {
      const sample = event.payload;
      samples = [...samples, sample];

      // Trim old samples - keep extra padding for smooth edge transitions
      const now = Date.now();
      const paddingMs = 500;
      samples = samples.filter(s => now - s.timestamp <= bufferDurationMs + paddingMs);
    });

    animationFrame = requestAnimationFrame(render);
  });

  onDestroy(() => {
    if (unlistenWaveform) unlistenWaveform();
    if (animationFrame) cancelAnimationFrame(animationFrame);
  });

  // Track time offset between performance.now() and Date.now() for smooth animation
  let timeOffset = Date.now() - performance.now();

  function render(rafTimestamp: number) {
    if (!ctx || !canvas) {
      animationFrame = requestAnimationFrame(render);
      return;
    }

    const w = canvas.width;
    const h = canvas.height;
    // Use RAF timestamp (converted to Date.now() scale) for smoother animation
    const now = timeOffset + rafTimestamp;

    // Clear canvas
    ctx.fillStyle = bgColor;
    ctx.fillRect(0, 0, w, h);

    // Draw grid and centerline
    drawGrid(w, h);

    if (samples.length > 0) {
      // Calculate max intensity across both channels for auto-scaling
      const maxIntensity = getMaxIntensity(now);

      // Draw waveforms - B first so A appears on top
      drawChannelWaveform(w, h, now, 'b', channelBColor, channelBColorAlpha, maxIntensity);
      drawChannelWaveform(w, h, now, 'a', channelAColor, channelAColorAlpha, maxIntensity);
    }

    animationFrame = requestAnimationFrame(render);
  }

  function drawGrid(w: number, h: number) {
    if (!ctx) return;

    ctx.strokeStyle = gridColor;
    ctx.lineWidth = 0.5;

    // Vertical time markers
    const timeStep = Math.max(200, Math.floor(bufferDurationMs / 5));
    const pixelsPerMs = w / bufferDurationMs;

    for (let t = 0; t <= bufferDurationMs; t += timeStep) {
      const x = w - (t * pixelsPerMs);
      ctx.beginPath();
      ctx.moveTo(x, 0);
      ctx.lineTo(x, h);
      ctx.stroke();
    }

    // Centerline
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(0, h / 2);
    ctx.lineTo(w, h / 2);
    ctx.stroke();

    // 25%/75% lines
    ctx.lineWidth = 0.5;
    for (let pct of [0.25, 0.75]) {
      ctx.beginPath();
      ctx.moveTo(0, h * pct);
      ctx.lineTo(w, h * pct);
      ctx.stroke();
    }
  }

  /**
   * Get max intensity across both channels in visible samples for auto-scaling
   */
  function getMaxIntensity(now: number): number {
    const drawPaddingMs = 200;
    let maxIntensity = 0;

    for (const sample of samples) {
      const age = now - sample.timestamp;
      if (age <= bufferDurationMs + drawPaddingMs && age >= -drawPaddingMs) {
        maxIntensity = Math.max(maxIntensity, sample.channel_a_intensity, sample.channel_b_intensity);
      }
    }

    // Use minimum of 10 to avoid scaling tiny signals too aggressively
    // Use 200 as the baseline so full intensity still fills the chart
    return Math.max(10, maxIntensity);
  }

  /**
   * Calculate the geometry for rendering cycles within a time slice
   */
  function calculateCycleGeometry(
    params: WaveformParams,
    h: number,
    maxAmplitude: number,
    scaleMaxIntensity: number
  ): CycleGeometry {
    // Scale intensity relative to max in buffer (auto-scaling)
    const normalizedIntensity = params.intensity / scaleMaxIntensity;
    const dutyRatio = params.freqBalance / 255;

    const canvasCenterY = h / 2;
    const amplitude = normalizedIntensity * maxAmplitude;

    // Int balance shifts the waveform's center point
    // 0 = center at top, 128 = center at canvas middle, 255 = center at bottom
    const balanceOffset = ((params.intBalance - 128) / 128) * maxAmplitude;
    const waveformCenterY = canvasCenterY + balanceOffset;

    return {
      cyclesPerSlice: params.frequency / 10, // cycles per 100ms sample
      dutyRatio,
      topY: waveformCenterY - amplitude,
      bottomY: waveformCenterY + amplitude,
      centerY: waveformCenterY,
      amplitude
    };
  }

  /**
   * Calculate amplitude multiplier for a position within a single cycle
   * Returns 0-1 where 1 = full amplitude, 0 = at center
   */
  function calculateWaveformAmplitude(
    t: number,           // 0-1 position within cycle
    dutyRatio: number    // 0-1 controls pulse width
  ): number {
    // Square wave: full amplitude during duty, zero during rest
    if (t < dutyRatio) {
      return 1.0;  // Full amplitude during "on" phase
    } else {
      return 0.0;  // At center during "off" phase
    }
  }

  /**
   * Get adaptive sample count based on pixel density
   */
  function getAdaptiveSampleCount(pixelsPerCycle: number): number {
    const densitySamples = Math.floor(pixelsPerCycle / 2);
    return Math.max(MIN_SAMPLES_PER_CYCLE, Math.min(MAX_SAMPLES_PER_CYCLE, densitySamples));
  }

  /**
   * Draw synthesized waveform for a channel
   */
  function drawChannelWaveform(
    w: number,
    h: number,
    now: number,
    channel: 'a' | 'b',
    strokeColor: string,
    fillColor: string,
    scaleMaxIntensity: number
  ) {
    if (!ctx || samples.length === 0) return;

    const pixelsPerMs = w / bufferDurationMs;
    const maxAmplitude = (h - 4) / 2;  // Half height for symmetric waves
    const drawPaddingMs = 200;

    // Sort samples by timestamp (oldest first)
    const sortedSamples = [...samples].sort((a, b) => a.timestamp - b.timestamp);

    // Filter samples within drawable range
    const drawableSamples = sortedSamples.filter(sample => {
      const age = now - sample.timestamp;
      return age <= bufferDurationMs + drawPaddingMs && age >= -drawPaddingMs;
    });

    if (drawableSamples.length === 0) return;

    // Extract params from sample
    const getParams = (sample: WaveformSample): WaveformParams => ({
      intensity: channel === 'a' ? sample.channel_a_intensity : sample.channel_b_intensity,
      frequency: channel === 'a' ? sample.channel_a_frequency : sample.channel_b_frequency,
      freqBalance: channel === 'a' ? sample.channel_a_freq_balance : sample.channel_b_freq_balance,
      intBalance: channel === 'a' ? sample.channel_a_int_balance : sample.channel_b_int_balance,
    });

    // Collect all path points with amplitude for symmetric rendering
    const pathPoints: { x: number; y: number; amp: number }[] = [];
    let cumulativePhase = 0;

    for (let i = 0; i < drawableSamples.length; i++) {
      const sample = drawableSamples[i];
      const nextSample = drawableSamples[i + 1];

      const age = now - sample.timestamp;
      const sampleX = w - (age * pixelsPerMs);  // X position of this sample

      // Calculate the X of the next sample (or extend 100ms to the right if last)
      const nextAge = nextSample
        ? (now - nextSample.timestamp)
        : Math.max(0, age - 100);
      const nextX = w - (nextAge * pixelsPerMs);

      // Slice goes from sampleX to nextX
      const sliceWidth = nextX - sampleX;

      if (sliceWidth <= 0) continue;

      const params = getParams(sample);
      const geometry = calculateCycleGeometry(params, h, maxAmplitude, scaleMaxIntensity);

      // Handle zero intensity - flat line at waveform's center (shifted by int_balance)
      if (params.intensity === 0) {
        pathPoints.push({ x: sampleX, y: geometry.centerY, amp: 0 });
        pathPoints.push({ x: nextX, y: geometry.centerY, amp: 0 });
        continue;
      }

      // Calculate cycles and pixels per cycle
      const totalCycles = geometry.cyclesPerSlice;
      const pixelsPerCycle = totalCycles > 0 ? sliceWidth / totalCycles : sliceWidth;

      // For very high frequencies or compressed view, show envelope-style
      if (pixelsPerCycle < MIN_PIXELS_PER_CYCLE && totalCycles > 1) {
        const oscCount = Math.max(1, Math.min(Math.ceil(totalCycles / 2), Math.floor(sliceWidth / 3)));
        const duty = geometry.dutyRatio;

        for (let osc = 0; osc < oscCount; osc++) {
          const oscStart = sampleX + (osc / oscCount) * sliceWidth;
          const oscEnd = sampleX + ((osc + 1) / oscCount) * sliceWidth;
          const oscMid = oscStart + (oscEnd - oscStart) * duty;

          // Full amplitude during duty, zero during rest
          pathPoints.push({ x: oscStart, y: geometry.centerY, amp: geometry.amplitude });
          pathPoints.push({ x: oscMid, y: geometry.centerY, amp: geometry.amplitude });
          pathPoints.push({ x: oscMid, y: geometry.centerY, amp: 0 });
          pathPoints.push({ x: oscEnd, y: geometry.centerY, amp: 0 });
        }
        continue;
      }

      // Generate detailed waveform for this time slice
      const samplesPerCycle = getAdaptiveSampleCount(pixelsPerCycle);
      const totalPoints = Math.max(1, Math.ceil(totalCycles * samplesPerCycle));

      for (let p = 0; p <= totalPoints; p++) {
        const progress = p / totalPoints;
        const pointX = sampleX + (progress * sliceWidth);

        const absolutePhase = cumulativePhase + (progress * totalCycles);
        const cyclePhase = absolutePhase % 1;

        const ampMultiplier = calculateWaveformAmplitude(cyclePhase, geometry.dutyRatio);
        const amp = geometry.amplitude * ampMultiplier;

        pathPoints.push({ x: pointX, y: geometry.centerY, amp });
      }

      cumulativePhase = (cumulativePhase + totalCycles) % 1;
    }

    if (pathPoints.length === 0) return;

    // Draw symmetric audio-wave style (grows from center)
    // Top edge path
    ctx.beginPath();
    ctx.moveTo(pathPoints[0].x, pathPoints[0].y - pathPoints[0].amp);
    for (let i = 1; i < pathPoints.length; i++) {
      ctx.lineTo(pathPoints[i].x, pathPoints[i].y - pathPoints[i].amp);
    }
    // Bottom edge path (reverse direction)
    for (let i = pathPoints.length - 1; i >= 0; i--) {
      ctx.lineTo(pathPoints[i].x, pathPoints[i].y + pathPoints[i].amp);
    }
    ctx.closePath();

    // Fill
    ctx.fillStyle = fillColor;
    ctx.fill();

    // Stroke top edge
    ctx.beginPath();
    ctx.moveTo(pathPoints[0].x, pathPoints[0].y - pathPoints[0].amp);
    for (let i = 1; i < pathPoints.length; i++) {
      ctx.lineTo(pathPoints[i].x, pathPoints[i].y - pathPoints[i].amp);
    }
    ctx.strokeStyle = strokeColor;
    ctx.lineWidth = 1.5;
    ctx.stroke();

    // Stroke bottom edge
    ctx.beginPath();
    ctx.moveTo(pathPoints[0].x, pathPoints[0].y + pathPoints[0].amp);
    for (let i = 1; i < pathPoints.length; i++) {
      ctx.lineTo(pathPoints[i].x, pathPoints[i].y + pathPoints[i].amp);
    }
    ctx.stroke();
  }

</script>

<div class="waveform-container relative">
  <canvas
    bind:this={canvas}
    width={400}
    height={height}
    class="waveform-canvas"
  />
  {#if samples.length === 0}
    <div class="absolute inset-0 flex items-center justify-center text-muted-foreground text-[10px]">
      Waiting for device output...
    </div>
  {/if}
</div>

<style>
  .waveform-container {
    width: 100%;
    border-radius: 6px;
    overflow: hidden;
  }

  .waveform-canvas {
    width: 100%;
    height: auto;
    display: block;
  }
</style>
