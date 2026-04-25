<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { ChevronUp, ChevronDown, Ban, Terminal, Activity } from 'lucide-svelte';
  import { writable } from 'svelte/store';
  import { invoke } from '@tauri-apps/api/core';
  import Tooltip from './ui/Tooltip.svelte';

  // Diagnostic capture state. When `captureActive` is true, the button
  // becomes a countdown / stop control. Auto-stop is enforced by the
  // backend; the UI just polls status to refresh the countdown + final
  // file path.
  let captureActive = false;
  let captureRemainingSec = 0;
  let captureStatusInterval: ReturnType<typeof setInterval> | null = null;
  const CAPTURE_DURATION_MS = 30_000;

  async function startDiagnosticCapture() {
    try {
      const path = await invoke<string>('start_diagnostic_capture', {
        durationMs: CAPTURE_DURATION_MS,
      });
      captureActive = true;
      captureRemainingSec = Math.ceil(CAPTURE_DURATION_MS / 1000);
      addLog('info', `Diagnostic capture started → ${path}`);
      // Poll status so the button reflects real backend state (auto-stop).
      captureStatusInterval = setInterval(refreshCaptureStatus, 500);
    } catch (e) {
      addLog('error', `Failed to start diagnostic capture: ${e}`);
    }
  }

  async function stopDiagnosticCapture() {
    try {
      const path = await invoke<string>('stop_diagnostic_capture');
      addLog('info', `Diagnostic capture saved → ${path}`);
    } catch (e) {
      addLog('error', `Failed to stop diagnostic capture: ${e}`);
    } finally {
      captureActive = false;
      captureRemainingSec = 0;
      if (captureStatusInterval) {
        clearInterval(captureStatusInterval);
        captureStatusInterval = null;
      }
    }
  }

  interface DiagnosticStatus {
    active: boolean;
    elapsed_ms: number;
    duration_ms: number;
    event_count: number;
    output_path: string | null;
  }

  async function refreshCaptureStatus() {
    try {
      const s = await invoke<DiagnosticStatus>('get_diagnostic_status');
      if (!s.active && captureActive) {
        // Backend auto-stopped (timer fired). Surface the file path.
        captureActive = false;
        captureRemainingSec = 0;
        if (captureStatusInterval) {
          clearInterval(captureStatusInterval);
          captureStatusInterval = null;
        }
        if (s.output_path) {
          addLog('info', `Diagnostic capture complete (${s.event_count} events) → ${s.output_path}`);
        }
      } else if (s.active) {
        captureRemainingSec = Math.max(0, Math.ceil((s.duration_ms - s.elapsed_ms) / 1000));
      }
    } catch (e) {
      // Silent: status polling shouldn't spam errors
    }
  }

  let isOpen = false;
  let panelHeight = 200;
  let isDragging = false;
  let startY = 0;
  let startHeight = 0;
  let logsContainer: HTMLDivElement;

  // Store for log messages
  export const logs = writable<Array<{
    timestamp: Date;
    level: 'info' | 'warn' | 'error' | 'debug';
    message: string;
  }>>([]);

  // Add log function
  export function addLog(level: 'info' | 'warn' | 'error' | 'debug', message: string) {
    logs.update(l => [...l, {
      timestamp: new Date(),
      level,
      message
    }]);

    // Auto-scroll to bottom
    if (logsContainer) {
      setTimeout(() => {
        logsContainer.scrollTop = logsContainer.scrollHeight;
      }, 0);
    }
  }

  // Format args for display, properly handling objects
  function formatArgs(args: any[]): string {
    return args.map(arg => {
      if (arg === null) return 'null';
      if (arg === undefined) return 'undefined';
      if (typeof arg === 'object') {
        try {
          return JSON.stringify(arg, null, 2);
        } catch {
          return String(arg);
        }
      }
      return String(arg);
    }).join(' ');
  }

  // Override console methods to capture logs + subscribe to backend log events
  onMount(() => {
    const originalLog = console.log;
    const originalWarn = console.warn;
    const originalError = console.error;
    const originalDebug = console.debug;

    console.log = (...args) => {
      originalLog(...args);
      addLog('info', formatArgs(args));
    };

    console.warn = (...args) => {
      originalWarn(...args);
      addLog('warn', formatArgs(args));
    };

    console.error = (...args) => {
      originalError(...args);
      addLog('error', formatArgs(args));
    };

    console.debug = (...args) => {
      originalDebug(...args);
      addLog('debug', formatArgs(args));
    };

    // Pipe backend ring-buffer log lines into the panel so Rust-side
    // activity (BLE notifications, device loop events, etc.) is visible.
    let unlistenBackend: (() => void) | null = null;
    (async () => {
      const { listen } = await import('@tauri-apps/api/event');
      const unlisten = await listen<{ level: string; message: string }>(
        'backend-log',
        (event) => {
          const lvl = (event.payload.level || 'info').toLowerCase();
          const mapped: 'info' | 'warn' | 'error' | 'debug' =
            lvl === 'warn' ? 'warn'
              : lvl === 'error' ? 'error'
                : lvl === 'debug' ? 'debug'
                  : 'info';
          addLog(mapped, event.payload.message);
        }
      );
      unlistenBackend = unlisten;
    })().catch((e) => originalError('[LogsPanel] Failed to subscribe to backend logs:', e));

    return () => {
      console.log = originalLog;
      console.warn = originalWarn;
      console.error = originalError;
      console.debug = originalDebug;
      if (unlistenBackend) unlistenBackend();
      if (captureStatusInterval) {
        clearInterval(captureStatusInterval);
        captureStatusInterval = null;
      }
    };
  });

  function startDrag(e: MouseEvent) {
    isDragging = true;
    startY = e.clientY;
    startHeight = panelHeight;
    document.addEventListener('mousemove', onDrag);
    document.addEventListener('mouseup', stopDrag);
  }

  function onDrag(e: MouseEvent) {
    if (!isDragging) return;
    const deltaY = startY - e.clientY;
    panelHeight = Math.max(100, Math.min(400, startHeight + deltaY));
  }

  function stopDrag() {
    isDragging = false;
    document.removeEventListener('mousemove', onDrag);
    document.removeEventListener('mouseup', stopDrag);
  }

  function clearLogs() {
    logs.set([]);
  }

  function getLevelColor(level: string) {
    switch (level) {
      case 'error': return 'text-red-500';
      case 'warn': return 'text-yellow-500';
      case 'debug': return 'text-muted-foreground';
      default: return 'text-foreground';
    }
  }
</script>

<!-- Logs Panel -->
<div
  class="fixed bottom-0 left-0 right-0 bg-card border-t border-border shadow-lg transition-transform z-50 {isOpen ? 'translate-y-0' : 'translate-y-full'}"
  style="height: {panelHeight}px"
>
  <!-- Drag Handle -->
  <div
    class="absolute top-0 left-0 right-0 h-1 bg-primary/20 hover:bg-primary/40 cursor-ns-resize"
    on:mousedown={startDrag}
  />

  <!-- Header -->
  <div class="flex items-center justify-between px-3 py-2 border-b border-border">
    <div class="flex items-center gap-2">
      <Terminal class="h-4 w-4 text-muted-foreground" />
      <span class="text-sm font-medium">Logs</span>
      <span class="text-xs text-muted-foreground">({$logs.length} entries)</span>
    </div>

    <div class="flex items-center gap-1">
      <Tooltip
        content={captureActive
          ? `Stop diagnostic capture (${captureRemainingSec}s left)`
          : 'Capture 30s of input + output to CSV for latency diagnosis'}
        placement="left"
      >
        <button
          class="px-2 py-1 hover:bg-muted rounded text-xs flex items-center gap-1
            {captureActive ? 'text-red-500' : 'text-muted-foreground hover:text-foreground'}"
          on:click={captureActive ? stopDiagnosticCapture : startDiagnosticCapture}
        >
          <Activity class="h-3 w-3 {captureActive ? 'animate-pulse' : ''}" />
          {#if captureActive}
            <span class="font-mono">{captureRemainingSec}s</span>
          {:else}
            <span>Capture</span>
          {/if}
        </button>
      </Tooltip>
      <Tooltip content="Clear Logs" placement="left">
        <button
          class="p-1 hover:bg-muted rounded text-muted-foreground hover:text-foreground"
          on:click={clearLogs}
        >
          <Ban class="h-3 w-3" />
        </button>
      </Tooltip>
      <button
        class="p-1 hover:bg-muted rounded text-muted-foreground hover:text-foreground"
        on:click={() => isOpen = false}
      >
        <ChevronDown class="h-3 w-3" />
      </button>
    </div>
  </div>

  <!-- Logs Content -->
  <div
    bind:this={logsContainer}
    class="overflow-y-auto p-2 font-mono text-xs"
    style="height: calc(100% - 40px)"
  >
    {#each $logs as log}
      <div class="flex gap-2 py-0.5">
        <span class="text-muted-foreground whitespace-nowrap">
          {log.timestamp.toLocaleTimeString()}
        </span>
        <span class="{getLevelColor(log.level)} uppercase w-12">
          [{log.level}]
        </span>
        <span class="flex-1 break-all">{log.message}</span>
      </div>
    {/each}
  </div>
</div>

<!-- Toggle Button -->
{#if !isOpen}
  <button
    class="fixed bottom-4 right-4 p-2 bg-card border border-border rounded-lg shadow-lg hover:bg-muted z-50"
    on:click={() => isOpen = true}
  >
    <div class="flex items-center gap-2">
      <Terminal class="h-4 w-4" />
      <span class="text-xs">Logs</span>
      {#if $logs.some(l => l.level === 'error')}
        <span class="h-2 w-2 bg-red-500 rounded-full animate-pulse" />
      {/if}
    </div>
  </button>
{/if}

<style>
  .cursor-ns-resize {
    cursor: ns-resize;
  }
</style>