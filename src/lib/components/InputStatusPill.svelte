<script lang="ts">
  import { Wifi, WifiOff, Plug, Gamepad2, RefreshCw } from 'lucide-svelte';
  import Popover from './ui/Popover.svelte';
  import ConnectionPanel from './ConnectionPanel.svelte';
  import { currentInputSource } from '$lib/stores/inputSource';

  interface Props {
    isConnected?: boolean;
    websocketPort?: number;
    autoOpen?: boolean;
    showTCodeMonitor?: boolean;
    onConnectionChange?: (connected: boolean) => void;
  }

  let {
    isConnected = $bindable(false),
    websocketPort = $bindable(12346),
    autoOpen = $bindable(true),
    showTCodeMonitor = $bindable(false),
    onConnectionChange = () => {}
  }: Props = $props();

  let popoverOpen = $state(false);
  let connectionPanel: ConnectionPanel = $state()!;

  export function getConnectionPanel() {
    return connectionPanel;
  }

  // Determine label and icon based on input source
  let inputSource = $derived($currentInputSource);
  let label = $derived(inputSource === 'tcode' ? 'T-Code' : inputSource === 'buttplug' ? 'Buttplug' : inputSource === 'lovense' ? 'Lovense' : 'Input');
  let icon = $derived(inputSource === 'tcode' ? Plug : inputSource === 'buttplug' ? Gamepad2 : inputSource === 'lovense' ? Gamepad2 : RefreshCw);
  let isSpinning = $derived(inputSource === 'none');
</script>

<Popover bind:open={popoverOpen} align="start">
  {#snippet trigger()}
    {@const SvelteComponent = icon}
  <button
      
      class="flex items-center gap-1.5 px-2.5 py-1 rounded-full text-xs font-medium transition-all
             {isConnected
               ? 'bg-green-500/20 text-green-400 border border-green-500/30 hover:bg-green-500/30'
               : 'bg-muted/50 text-muted-foreground border border-border hover:bg-muted'}"
    >
      <SvelteComponent class="h-3 w-3 {isSpinning ? 'animate-spin' : ''}" />
      <span>{label}</span>
      <span class="w-1.5 h-1.5 rounded-full {isConnected ? 'bg-green-400' : 'bg-muted-foreground/50'}"></span>
    </button>
  {/snippet}

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
