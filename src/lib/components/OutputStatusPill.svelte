<script lang="ts">
  import { Bluetooth, BluetoothOff, Battery, BatteryLow, BatteryMedium, BatteryFull, BatteryWarning } from 'lucide-svelte';
  import Popover from './ui/Popover.svelte';
  import BluetoothPanel from './BluetoothPanel.svelte';

  interface BluetoothDevice {
    address: string;
    name?: string;
    product?: string;
    rssi?: number;
  }

  interface Props {
    isConnected?: boolean;
    batteryLevel?: number | null;
    selectedInterface?: number;
    autoScan?: boolean;
    autoConnect?: boolean;
    savedDevices?: BluetoothDevice[];
    savedSelectedDevice?: string;
    onConnectionChange?: (connected: boolean) => void;
  }

  let {
    isConnected = $bindable(false),
    batteryLevel = null,
    selectedInterface = $bindable(0),
    autoScan = $bindable(true),
    autoConnect = $bindable(true),
    savedDevices = [],
    savedSelectedDevice = '',
    onConnectionChange = () => {}
  }: Props = $props();

  let popoverOpen = $state(false);
  let bluetoothPanel: BluetoothPanel = $state()!;

  export function getBluetoothPanel() {
    return bluetoothPanel;
  }

  function getBatteryIcon(level: number | null) {
    if (level === null) return Battery;
    if (level <= 10) return BatteryWarning;
    if (level <= 25) return BatteryLow;
    if (level <= 75) return BatteryMedium;
    return BatteryFull;
  }

  function getBatteryColor(level: number | null) {
    if (level === null) return 'text-muted-foreground';
    if (level <= 10) return 'text-red-500';
    if (level <= 25) return 'text-orange-500';
    if (level <= 50) return 'text-yellow-500';
    return 'text-green-500';
  }
</script>

<Popover bind:open={popoverOpen} align="start">
  {#snippet trigger()}
    <button
      
      class="flex items-center gap-1.5 px-2.5 py-1 rounded-full text-xs font-medium transition-all
             {isConnected
               ? 'bg-green-500/20 text-green-400 border border-green-500/30 hover:bg-green-500/30'
               : 'bg-muted/50 text-muted-foreground border border-border hover:bg-muted'}"
    >
      {#if isConnected}
        <Bluetooth class="h-3 w-3" />
      {:else}
        <BluetoothOff class="h-3 w-3" />
      {/if}
      <span>Output</span>
      {#if isConnected && batteryLevel !== null}
        {@const SvelteComponent = getBatteryIcon(batteryLevel)}
      <span class="flex items-center gap-0.5 pl-1 border-l border-green-500/30 ml-0.5 {getBatteryColor(batteryLevel)}">
          <SvelteComponent class="h-3 w-3" />
          <span class="text-[10px]">{batteryLevel}%</span>
        </span>
      {:else}
        <span class="w-1.5 h-1.5 rounded-full {isConnected ? 'bg-green-400' : 'bg-muted-foreground/50'}"></span>
      {/if}
    </button>
  {/snippet}

  <div class="space-y-3">
    <div class="flex items-center justify-between">
      <h3 class="text-sm font-medium">Output Connection</h3>
      <span class="text-xs {isConnected ? 'text-green-400' : 'text-muted-foreground'}">
        {isConnected ? 'Connected' : 'Disconnected'}
      </span>
    </div>

    <BluetoothPanel
      bind:this={bluetoothPanel}
      compact={true}
      bind:selectedInterface
      bind:autoScan
      bind:autoConnect
      {savedDevices}
      {savedSelectedDevice}
      bind:isConnected
      {onConnectionChange}
    />
  </div>
</Popover>
