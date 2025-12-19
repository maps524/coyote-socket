<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { ChevronUp, ChevronDown, Ban, Terminal } from 'lucide-svelte';
  import { writable } from 'svelte/store';
  import Tooltip from './ui/Tooltip.svelte';

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

  // Override console methods to capture logs
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

    return () => {
      console.log = originalLog;
      console.warn = originalWarn;
      console.error = originalError;
      console.debug = originalDebug;
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