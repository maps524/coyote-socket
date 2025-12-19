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
    channel_a_freq_balance: number;   // 0-255
    channel_b_freq_balance: number;   // 0-255
    channel_a_int_balance: number;    // 0-255
    channel_b_int_balance: number;    // 0-255
    channel_a_waveform: number[];     // [4] 0-100
    channel_b_waveform: number[];     // [4] 0-100
  }

  // State
  let canvas: HTMLCanvasElement;
  let ctx: CanvasRenderingContext2D | null = null;
  let animationFrame: number | null = null;
  let unlistenWaveform: UnlistenFn | null = null;

  let samples: WaveformSample[] = [];

  // Theme colors (matching CSS variables)
  // Channel A = purple (primary), Channel B = blue (secondary)
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

    render();
  });

  onDestroy(() => {
    if (unlistenWaveform) unlistenWaveform();
    if (animationFrame) cancelAnimationFrame(animationFrame);
  });

  function render() {
    if (!ctx || !canvas) {
      animationFrame = requestAnimationFrame(render);
      return;
    }

    const w = canvas.width;
    const h = canvas.height;
    const now = Date.now();

    // Clear canvas
    ctx.fillStyle = bgColor;
    ctx.fillRect(0, 0, w, h);

    // Draw grid and centerline
    drawGrid(w, h);

    if (samples.length > 0) {
      // Calculate max intensity across both channels for auto-scaling
      const maxIntensity = getMaxIntensity(now);

      // Draw waveforms using sample data directly
      // Draw B first so A appears on top
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
    return Math.max(10, maxIntensity);
  }

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
    const centerY = h / 2;
    const maxAmplitude = h - 4;  // Full canvas height available for waveform

    // Sort samples by timestamp for proper line drawing
    const sortedSamples = [...samples].sort((a, b) => a.timestamp - b.timestamp);

    // Allow drawing slightly beyond visible buffer for smooth edge transitions
    const drawPaddingMs = 200;

    // Helper to get Y values for a sample
    const getYValues = (sample: WaveformSample) => {
      const intensity = channel === 'a' ? sample.channel_a_intensity : sample.channel_b_intensity;
      const intBalance = channel === 'a' ? sample.channel_a_int_balance : sample.channel_b_int_balance;
      // Scale intensity relative to max in buffer (auto-scaling)
      const normalizedIntensity = intensity / scaleMaxIntensity;
      const upRatio = 1 - (intBalance / 255);
      const downRatio = intBalance / 255;
      const upHeight = normalizedIntensity * maxAmplitude * upRatio;
      const downHeight = normalizedIntensity * maxAmplitude * downRatio;
      return {
        topY: centerY - upHeight,
        bottomY: centerY + downHeight
      };
    };

    // Filter samples within drawable range
    const drawableSamples = sortedSamples.filter(sample => {
      const age = now - sample.timestamp;
      return age <= bufferDurationMs + drawPaddingMs && age >= -drawPaddingMs;
    });

    if (drawableSamples.length === 0) return;

    // Build path for filled area - draw from newest to oldest (right to left)
    ctx.beginPath();

    // Start with top edge
    let firstPoint = true;
    for (let i = drawableSamples.length - 1; i >= 0; i--) {
      const sample = drawableSamples[i];
      const age = now - sample.timestamp;
      const x = w - (age * pixelsPerMs);
      const { topY } = getYValues(sample);

      if (firstPoint) {
        ctx.moveTo(x, topY);
        firstPoint = false;
      } else {
        ctx.lineTo(x, topY);
      }
    }

    // Continue with bottom edge from oldest to newest (left to right)
    for (let i = 0; i < drawableSamples.length; i++) {
      const sample = drawableSamples[i];
      const age = now - sample.timestamp;
      const x = w - (age * pixelsPerMs);
      const { bottomY } = getYValues(sample);
      ctx.lineTo(x, bottomY);
    }

    ctx.closePath();
    ctx.fillStyle = fillColor;
    ctx.fill();

    // Draw stroke on top edge
    ctx.beginPath();
    firstPoint = true;
    for (let i = drawableSamples.length - 1; i >= 0; i--) {
      const sample = drawableSamples[i];
      const age = now - sample.timestamp;
      const x = w - (age * pixelsPerMs);
      const { topY } = getYValues(sample);
      if (firstPoint) {
        ctx.moveTo(x, topY);
        firstPoint = false;
      } else {
        ctx.lineTo(x, topY);
      }
    }
    ctx.strokeStyle = strokeColor;
    ctx.lineWidth = 1.5;
    ctx.stroke();

    // Draw bottom edge stroke
    ctx.beginPath();
    firstPoint = true;
    for (let i = drawableSamples.length - 1; i >= 0; i--) {
      const sample = drawableSamples[i];
      const age = now - sample.timestamp;
      const x = w - (age * pixelsPerMs);
      const { bottomY } = getYValues(sample);
      if (firstPoint) {
        ctx.moveTo(x, bottomY);
        firstPoint = false;
      } else {
        ctx.lineTo(x, bottomY);
      }
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
