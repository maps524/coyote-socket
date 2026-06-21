<script lang="ts" context="module">
  export interface BluetoothDevice {
    address: string;
    name?: string;
    product?: string;
    rssi?: number;
  }

  export interface BluetoothPanelState {
    devices: BluetoothDevice[];
    selectedDevice: string;
  }
</script>

<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { RefreshCw } from 'lucide-svelte';
  import Button from './ui/Button.svelte';
  import Select from './ui/Select.svelte';
  import Toggle from './ui/Toggle.svelte';
  import StatusIndicator from './ui/StatusIndicator.svelte';

  export let compact = false;
  export let selectedInterface = 0;
  export let autoScan = true;
  export let autoConnect = true;  // Auto-connect when device found
  export let onConnectionChange = (connected: boolean) => {};
  export let savedDevices: BluetoothDevice[] = [];
  export let savedSelectedDevice = '';
  export let isConnected = false;  // Now a bindable prop from parent

  let bluetoothAdapters: string[] = [];
  let selectedDevice = savedSelectedDevice;
  let connectionStatus = '';
  let adaptersLoaded = false;
  let scanActive = false; // backend scan session running for this panel

  // Devices are owned by the backend scan loop now; mirror the live,
  // store-backed `savedDevices` prop straight through to the dropdown.
  $: bluetoothDevices = savedDevices;

  // Auto-select the first Coyote once one appears and nothing is chosen yet.
  $: if (!selectedDevice && bluetoothDevices.length > 0) {
    const coyote = bluetoothDevices.find(d =>
      d.name?.includes('COYOTE') || d.name?.includes('DG-LAB') || d.name?.includes('47L')
    );
    if (coyote) selectedDevice = coyote.address;
  }

  $: if (isConnected && selectedDevice) {
    const device = bluetoothDevices.find(d => d.address === selectedDevice);
    if (device) {
      connectionStatus = `Connected to ${getDeviceDisplayName(device)}`;
    }
  }

  // A usable adapter is one the OS actually reported (not the placeholder
  // entry we substitute when btleplug finds nothing).
  $: hasValidAdapter =
    adaptersLoaded && bluetoothAdapters.length > 0 && bluetoothAdapters[0] !== 'No adapters found';

  // Placeholder text shown inside the device dropdown while it's empty.
  $: deviceStatusText = !adaptersLoaded
    ? 'Waiting for interface…'
    : !hasValidAdapter
      ? 'No Bluetooth interface'
      : scanActive
        ? 'Scanning…'
        : 'No devices found';

  // Live activity state for the dot indicator next to the device label. The
  // dot pulses the whole time the backend scan is active, so it reads as
  // continuously scanning rather than flickering.
  $: scanState = !adaptersLoaded
    ? { label: 'Waiting for interface', state: 'idle' as const }
    : !hasValidAdapter
      ? { label: 'No interface', state: 'idle' as const }
      : isConnected
        ? { label: 'Connected', state: 'active' as const }
        : scanActive
          ? { label: 'Scanning', state: 'scanning' as const }
          : autoScan
            ? { label: 'Starting…', state: 'scanning' as const }
            : { label: 'Auto-scan off', state: 'idle' as const };

  // --- Backend-owned scanning ----------------------------------------------
  // The backend runs the real scan loop and pushes `devices-discovered`
  // events (mirrored into the store → `savedDevices`). This panel just turns
  // that loop on while it's mounted/enabled and off otherwise — no frontend
  // timer. In compact mode "mounted" means the output popover is open.
  async function startScan() {
    if (scanActive || !hasValidAdapter || isConnected) return;
    scanActive = true; // set synchronously so the reactive can't double-fire
    const adapterIndex = Number(selectedInterface) || 0;
    try {
      await invoke('start_device_scan', { adapterIndex });
    } catch (error) {
      scanActive = false;
      console.error('Failed to start device scan:', error);
    }
  }

  async function stopScan() {
    if (!scanActive) return;
    scanActive = false;
    try {
      await invoke('stop_device_scan');
    } catch (error) {
      console.error('Failed to stop device scan:', error);
    }
  }

  // Drive the backend scan from live state: scan while enabled + idle, stop
  // once connected or the user disables auto-scan. This is what makes the
  // auto-scan toggle visibly do something.
  $: if (adaptersLoaded) {
    if (autoScan && hasValidAdapter && !isConnected) {
      startScan();
    } else {
      stopScan();
    }
  }

  onDestroy(() => {
    stopScan();
  });

  onMount(async () => {
    // Load adapters; the reactive above starts the backend scan once loaded.
    await loadBTAdapters();
  });
  
  // Export current state for parent to save. Devices are backend-owned, so we
  // only carry the user's selection here.
  export function getState(): BluetoothPanelState {
    return {
      devices: bluetoothDevices,
      selectedDevice
    };
  }

  // Export scan trigger for parent — ensures adapters are loaded, then kicks
  // the backend scan loop.
  export async function triggerScan() {
    if (!adaptersLoaded) {
      await loadBTAdapters();
    }
    await startScan();
  }

  // Export function to check if adapters are loaded
  export function isAdaptersLoaded() {
    return adaptersLoaded;
  }

  async function loadBTAdapters() {
    try {
      const adapters = await invoke<string[]>('get_bluetooth_adapters');
      bluetoothAdapters = adapters.length > 0 ? adapters : ['No adapters found'];
      adaptersLoaded = true;

      // Ensure selectedInterface is within bounds
      const currentIndex = Number(selectedInterface) || 0;
      if (currentIndex >= bluetoothAdapters.length) {
        selectedInterface = 0;
      }
      console.log('Bluetooth adapters loaded:', bluetoothAdapters, 'selectedInterface:', selectedInterface);
    } catch (error) {
      console.error('Failed to load Bluetooth adapters:', error);
      bluetoothAdapters = ['No adapters found'];
      adaptersLoaded = true;
    }
  }
  
  // Manual refresh: (re)target the backend scan on the current adapter. With
  // continuous scanning this is rarely needed, but the button stays for
  // familiarity and to recover if a scan was stopped (e.g. after disconnect).
  async function refreshScan() {
    if (isConnected) return;
    const adapterIndex = Number(selectedInterface) || 0;
    try {
      await invoke('start_device_scan', { adapterIndex });
      scanActive = true;
    } catch (error) {
      console.error('Failed to refresh scan:', error);
    }
  }

  export async function connectDevice() {
    if (isConnected) {
      // Disconnect
      try {
        const result = await invoke<string>('disconnect_bluetooth_device');
        connectionStatus = result;
        isConnected = false;
        onConnectionChange(false);
        console.log('Output (Bluetooth) connection closed');
      } catch (error) {
        connectionStatus = `Disconnect failed: ${error}`;
        console.error('Disconnect failed:', error);
      }
    } else {
      // Connect
      if (!selectedDevice) {
        connectionStatus = 'No device selected';
        return;
      }

      // Ensure adapterIndex is a valid number
      const adapterIndex = typeof selectedInterface === 'number' ? selectedInterface : parseInt(String(selectedInterface), 10);
      if (isNaN(adapterIndex) || adapterIndex < 0) {
        connectionStatus = 'Invalid adapter selected';
        console.error('Invalid adapterIndex:', selectedInterface);
        return;
      }

      console.log('Connecting with adapterIndex:', adapterIndex, 'address:', selectedDevice);

      try {
        const result = await invoke<string>('connect_bluetooth_device', {
          adapterIndex: adapterIndex,
          address: selectedDevice
        });
        connectionStatus = result;
        isConnected = true;
        onConnectionChange(true);
        console.log('Output (Bluetooth) connection established');
      } catch (error) {
        connectionStatus = `Connection failed: ${error}`;
        console.error('Connection failed:', error);
      }
    }
  }

  function getDeviceDisplayName(device: BluetoothDevice): string {
    // Prefer the friendly product label (e.g. "Coyote 3.0"), appending the
    // raw advertised id when it adds information; fall back to name, then
    // address when the peripheral advertised nothing useful.
    if (device.product) {
      const id = device.name && device.name !== device.product ? ` · ${device.name}` : '';
      return `${device.product}${id}`;
    }
    return device.name || device.address;
  }

  async function handleInterfaceChange(event: Event) {
    // HTML select converts values to strings, so convert back to number
    const target = event.target as HTMLSelectElement;
    const newIndex = parseInt(target.value, 10);
    selectedInterface = isNaN(newIndex) ? 0 : newIndex;
    console.log('Interface changed to:', selectedInterface, 'type:', typeof selectedInterface);

    // Reset device selection when changing interface
    selectedDevice = '';

    // Retarget the backend scan onto the newly selected adapter (it reads the
    // target live, so a re-issue is enough — no need to stop first).
    if (!isConnected && selectedInterface >= 0 && selectedInterface < bluetoothAdapters.length) {
      const adapterIndex = Number(selectedInterface) || 0;
      try {
        await invoke('start_device_scan', { adapterIndex });
        scanActive = true;
      } catch (error) {
        console.error('Failed to retarget scan:', error);
      }
    }
  }
</script>

<div class="{compact ? '' : 'bg-card border rounded-lg p-4'}">
  {#if !compact}
    <h2 class="text-xl font-semibold mb-4">Bluetooth Connection</h2>
  {/if}
  
  <div class="space-y-4">
    <!-- Bluetooth Adapter Selection -->
    <div class="space-y-2">
      <label class="text-sm font-medium">Available Bluetooth Interfaces</label>
      <div class="flex gap-2">
        <select
          value={selectedInterface}
          on:change={handleInterfaceChange}
          class="flex h-10 w-full rounded-md border border-input bg-background text-foreground pl-3 pr-10 py-2 text-sm ring-offset-background appearance-none focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50 flex-1"
          style="background-image: url(&quot;data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='12' height='12' viewBox='0 0 12 12'%3E%3Cpath fill='%23888' d='M10.293 3.293L6 7.586 1.707 3.293A1 1 0 00.293 4.707l5 5a1 1 0 001.414 0l5-5a1 1 0 10-1.414-1.414z'/%3E%3C/svg%3E&quot;); background-repeat: no-repeat; background-position: right 0.7rem center; background-size: 12px;"
        >
          {#if bluetoothAdapters.length === 0}
            <option value={0}>Loading adapters...</option>
          {:else}
            {#each bluetoothAdapters as adapter, index}
              <option value={index} selected={selectedInterface === index}>{adapter}</option>
            {/each}
          {/if}
        </select>
        <Button
          variant="outline"
          size="icon"
          on:click={loadBTAdapters}
          disabled={isConnected}
          class="h-10 w-10"
        >
          <RefreshCw class="h-4 w-4" />
        </Button>
      </div>
    </div>

    <!-- Found Devices -->
    <div class="space-y-2">
      <div class="flex items-center justify-between">
        <label class="text-sm font-medium">Bluetooth Devices Found</label>
        <StatusIndicator label={scanState.label} state={scanState.state} />
      </div>
      <div class="flex gap-2">
        <Select bind:value={selectedDevice} disabled={bluetoothDevices.length === 0} class="flex-1">
          {#if bluetoothDevices.length === 0}
            <option value="">{deviceStatusText}</option>
          {:else}
            {#each bluetoothDevices as device}
              <option value={device.address}>{getDeviceDisplayName(device)}</option>
            {/each}
          {/if}
        </Select>
        <Button
          variant="outline"
          size="icon"
          on:click={refreshScan}
          disabled={isConnected}
          class="h-10 w-10"
        >
          <RefreshCw class="h-4 w-4 {scanActive ? 'animate-spin' : ''}" />
        </Button>
      </div>
    </div>

    {#if !compact}
      <!-- Action Buttons -->
      <div class="grid grid-cols-2 gap-2">
        <Button
          on:click={refreshScan}
          disabled={isConnected}
          variant="outline"
        >
          {scanActive ? 'Scanning…' : 'Scan for Devices'}
        </Button>
        
        <Button 
          on:click={connectDevice}
          disabled={!selectedDevice}
          variant={isConnected ? 'destructive' : 'default'}
        >
          {isConnected ? 'Disconnect Output' : 'Connect Output'}
        </Button>
      </div>
    {/if}
    
    <!-- Auto-scan and auto-connect options for settings -->
    {#if compact}
      <div class="space-y-2">
        <label class="flex items-center space-x-2 cursor-pointer">
          <Toggle bind:checked={autoScan} />
          <span class="text-sm">Auto-scan for Coyote on startup</span>
        </label>
        <label class="flex items-center space-x-2 cursor-pointer">
          <Toggle bind:checked={autoConnect} />
          <span class="text-sm">Auto-connect when device found</span>
        </label>
      </div>

      <!-- Connection Button for compact mode -->
      <Button
        on:click={connectDevice}
        disabled={!selectedDevice}
        variant={isConnected ? 'destructive' : 'default'}
        size="sm"
        class="w-full"
      >
        {isConnected ? 'Disconnect Output' : 'Connect Output'}
      </Button>
    {/if}

    <!-- Connection Status -->
    {#if connectionStatus}
      <div class="text-sm p-2 rounded {isConnected ? 'bg-green-500/10 border border-green-500/20 text-green-600' : 'bg-secondary'}">
        {connectionStatus}
      </div>
    {/if}
  </div>
</div>