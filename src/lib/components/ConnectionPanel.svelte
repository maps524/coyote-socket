<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import Button from './ui/Button.svelte';
  import PortInput from './ui/PortInput.svelte';
  import Toggle from './ui/Toggle.svelte';
  import { generalSettings } from '$lib/stores/generalSettings.js';

  export let compact = false;
  export let autoOpen = true;
  export let showTCodeMonitor = true;
  export let onConnectionChange = (connected: boolean) => {};
  export let isConnected = false;

  let connectionStatus = '';
  let port = 12346;
  let isLoading = true;

  // Load port from backend settings on mount
  onMount(async () => {
    try {
      const savedPort = await invoke<number>('get_websocket_port');
      port = savedPort;
    } catch (error) {
      console.error('Failed to load port from settings:', error);
    }
    isLoading = false;
  });

  // Handler for T-Code monitor toggle that updates the store
  function handleTCodeMonitorToggle(event: CustomEvent<boolean>) {
    generalSettings.update(s => ({ ...s, showTCodeMonitor: event.detail }));
  }

  // Save port to backend when it changes
  async function handlePortChange(newPort: number) {
    port = newPort;
    try {
      await invoke('save_connection_settings', {
        websocketPort: port,
        autoOpen,
        showTcodeMonitor: showTCodeMonitor,
      });
    } catch (error) {
      console.error('Failed to save port:', error);
    }
  }

  // Export function for parent to trigger auto-connect
  export async function triggerAutoConnect() {
    if (!isConnected) {
      await connect();
    }
  }

  export async function toggleConnection() {
    if (isConnected) {
      await disconnect();
    } else {
      await connect();
    }
  }

  async function connect() {
    try {
      const result = await invoke<string>('start_websocket_server', {
        port
      });
      connectionStatus = result;
      isConnected = true;
      onConnectionChange(true);
      console.log('Input connection established on port', port);
    } catch (error) {
      connectionStatus = `Connection failed: ${error}`;
      console.error('Connection failed:', error);
    }
  }

  async function disconnect() {
    try {
      const result = await invoke<string>('stop_websocket_server');
      connectionStatus = result;
      isConnected = false;
      onConnectionChange(false);
      console.log('WebSocket server stopped');
    } catch (error) {
      connectionStatus = `Disconnect failed: ${error}`;
      console.error('Disconnect failed:', error);
    }
  }
</script>

<div class="{compact ? '' : 'bg-card border rounded-lg p-4'}">
  {#if !compact}
    <h2 class="text-xl font-semibold mb-4">Connection Settings</h2>
  {/if}

  <div class="space-y-3">
    <!-- WebSocket Port -->
    <div class="space-y-1.5">
      <label class="text-xs text-muted-foreground">WebSocket URL</label>
      {#if isLoading}
        <div class="h-10 bg-muted animate-pulse rounded-md"></div>
      {:else}
        <PortInput
          bind:port
          disabled={isConnected}
          on:change={(e) => handlePortChange(e.detail)}
        />
      {/if}
    </div>

    {#if compact}
      <!-- Settings toggles -->
      <div class="space-y-2">
        <label class="flex items-center space-x-2 cursor-pointer">
          <Toggle bind:checked={autoOpen} />
          <span class="text-sm">Auto-open on startup</span>
        </label>
        <label class="flex items-center space-x-2 cursor-pointer">
          <Toggle checked={showTCodeMonitor} on:change={handleTCodeMonitorToggle} />
          <span class="text-sm">Show Input Monitor</span>
        </label>
      </div>

      <!-- Connection Button for compact mode -->
      <Button
        on:click={toggleConnection}
        variant={isConnected ? 'destructive' : 'default'}
        size="sm"
        class="w-full"
      >
        {isConnected ? 'Close Input' : 'Open Input'}
      </Button>
    {/if}

    {#if !compact && !autoOpen}
      <!-- Connection Button -->
      <Button
        on:click={toggleConnection}
        variant={isConnected ? 'destructive' : 'default'}
        class="w-full"
      >
        {isConnected ? 'Close Input' : 'Open Input'}
      </Button>
    {/if}

    <!-- Connection Status -->
    {#if connectionStatus}
      <div class="text-sm p-2 rounded bg-muted/50 text-muted-foreground">
        {connectionStatus}
      </div>
    {/if}
  </div>
</div>
