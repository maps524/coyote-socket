<script lang="ts">
  import { Wifi, WifiOff, Plug, Gamepad2, RefreshCw } from 'lucide-svelte';
  import Popover from './ui/Popover.svelte';
  import ConnectionPanel from './ConnectionPanel.svelte';
  import { currentInputSource } from '$lib/stores/inputSource';

  export let isConnected = false;
  export let websocketPort = 12346;
  export let autoOpen = true;
  export let showTCodeMonitor = false;
  export let onConnectionChange: (connected: boolean) => void = () => {};

  let popoverOpen = false;
  let connectionPanel: ConnectionPanel;

  export function getConnectionPanel() {
    return connectionPanel;
  }

  // Determine label and icon based on input source
  $: inputSource = $currentInputSource;
  $: label = inputSource === 'tcode' ? 'T-Code' : inputSource === 'buttplug' ? 'Buttplug' : inputSource === 'lovense' ? 'Lovense' : 'Input';
  $: icon = inputSource === 'tcode' ? Plug : inputSource === 'buttplug' ? Gamepad2 : inputSource === 'lovense' ? Gamepad2 : RefreshCw;
  $: isSpinning = inputSource === 'none';
</script>

<Popover bind:open={popoverOpen} align="start">
  <button
    slot="trigger"
    class="flex items-center gap-1.5 px-2.5 py-1 rounded-full text-xs font-medium transition-all
           {isConnected
             ? 'bg-green-500/20 text-green-400 border border-green-500/30 hover:bg-green-500/30'
             : 'bg-muted/50 text-muted-foreground border border-border hover:bg-muted'}"
  >
    <svelte:component this={icon} class="h-3 w-3 {isSpinning ? 'animate-spin' : ''}" />
    <span>{label}</span>
    <span class="w-1.5 h-1.5 rounded-full {isConnected ? 'bg-green-400' : 'bg-muted-foreground/50'}"></span>
  </button>

  <div class="space-y-3">
    <div class="flex items-center justify-between">
      <h3 class="text-sm font-medium">Input Connection</h3>
      <span class="text-xs {isConnected ? 'text-green-400' : 'text-muted-foreground'}">
        {isConnected ? 'Connected' : 'Disconnected'}
      </span>
    </div>

    <ConnectionPanel
      bind:this={connectionPanel}
      compact={true}
      bind:autoOpen
      bind:showTCodeMonitor
      bind:isConnected
      {onConnectionChange}
    />
  </div>
</Popover>
