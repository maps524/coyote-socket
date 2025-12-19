<script lang="ts" context="module">
  export interface BluetoothDevice {
    address: string;
    name?: string;
    rssi?: number;
  }

  export interface BluetoothPanelState {
    devices: BluetoothDevice[];
    selectedDevice: string;
  }
</script>

<script lang="ts">
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { RefreshCw } from 'lucide-svelte';
  import Button from './ui/Button.svelte';
  import Select from './ui/Select.svelte';
  import Toggle from './ui/Toggle.svelte';

  export let compact = false;
  export let selectedInterface = 0;
  export let autoScan = true;
  export let autoConnect = true;  // Auto-connect when device found
  export let onConnectionChange = (connected: boolean) => {};
  export let savedDevices: BluetoothDevice[] = [];
  export let savedSelectedDevice = '';
  export let isConnected = false;  // Now a bindable prop from parent

  let bluetoothAdapters: string[] = [];
  let bluetoothDevices: BluetoothDevice[] = savedDevices;
  let selectedDevice = savedSelectedDevice;
  let isScanning = false;
  let connectionStatus = '';
  let adaptersLoaded = false;
  
  $: if (isConnected && selectedDevice) {
    const device = bluetoothDevices.find(d => d.address === selectedDevice);
    if (device) {
      connectionStatus = `Connected to ${device.name || 'Unknown Device'} (${device.address})`;
    }
  }

  onMount(async () => {
    // Load Bluetooth adapters on mount
    await loadBTAdapters();
    
    // Restore saved devices if we have them
    if (savedDevices.length > 0) {
      bluetoothDevices = savedDevices;
      selectedDevice = savedSelectedDevice;
    }
    
    // Auto-scan if enabled and we have a valid adapter selected
    if (autoScan && !compact && bluetoothAdapters.length > 0 && selectedInterface !== null && selectedInterface !== undefined) {
      await scanForDevices();
    }
  });
  
  // Export current state for parent to save
  export function getState(): BluetoothPanelState {
    return {
      devices: bluetoothDevices,
      selectedDevice
    };
  }

  // Export scan function for parent to trigger
  export async function triggerScan() {
    if (!adaptersLoaded) {
      await loadBTAdapters();
    }
    if (bluetoothAdapters.length > 0 && bluetoothAdapters[0] !== 'No adapters found') {
      await scanForDevices();
    }
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
  
  async function scanForDevices() {
    isScanning = true;
    connectionStatus = 'Scanning for devices...';
    
    // Validate selectedInterface
    console.log('selectedInterface value:', selectedInterface, 'type:', typeof selectedInterface);
    const adapterIndex = selectedInterface === null || selectedInterface === undefined ? 0 : Number(selectedInterface);
    console.log('adapterIndex after conversion:', adapterIndex);
    
    if (isNaN(adapterIndex) || adapterIndex < 0) {
      console.error('Invalid adapter index:', selectedInterface);
      connectionStatus = 'Please select a Bluetooth adapter';
      isScanning = false;
      return;
    }
    
    try {
      bluetoothDevices = await invoke<BluetoothDevice[]>('scan_bluetooth_devices', {
        adapterIndex: adapterIndex
      });
      
      if (bluetoothDevices.length === 0) {
        connectionStatus = 'No DG-LAB devices found';
      } else {
        connectionStatus = `Found ${bluetoothDevices.length} device${bluetoothDevices.length > 1 ? 's' : ''}`;
        
        // Auto-select first Coyote device found
        const coyoteDevice = bluetoothDevices.find(d => 
          d.name?.includes('COYOTE') || 
          d.name?.includes('DG-LAB') || 
          d.name?.includes('47L')
        );
        if (coyoteDevice) {
          selectedDevice = coyoteDevice.address;
        }
      }
    } catch (error) {
      console.error('Failed to scan for devices:', error);
      connectionStatus = `Scan failed: ${error}`;
      bluetoothDevices = [];
    } finally {
      isScanning = false;
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
    const name = device.name || 'Unknown Device';
    return `${name} - ${device.address}`;
  }

  async function handleInterfaceChange(event: Event) {
    // HTML select converts values to strings, so convert back to number
    const target = event.target as HTMLSelectElement;
    const newIndex = parseInt(target.value, 10);
    selectedInterface = isNaN(newIndex) ? 0 : newIndex;
    console.log('Interface changed to:', selectedInterface, 'type:', typeof selectedInterface);

    // Reset device selection when changing interface
    selectedDevice = '';
    bluetoothDevices = [];

    // Auto-scan on interface change if valid interface is selected
    if (bluetoothAdapters.length > 0 && selectedInterface >= 0 && selectedInterface < bluetoothAdapters.length) {
      await scanForDevices();
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
      <label class="text-sm font-medium">Available Bluetooth Interfaces:</label>
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
          disabled={isScanning}
          class="h-10 w-10"
        >
          <RefreshCw class="h-4 w-4" />
        </Button>
      </div>
    </div>

    <!-- Found Devices -->
    <div class="space-y-2">
      <label class="text-sm font-medium">Bluetooth Devices Found:</label>
      <div class="flex gap-2">
        <Select bind:value={selectedDevice} disabled={bluetoothDevices.length === 0} class="flex-1">
          {#if bluetoothDevices.length === 0}
            <option value="">No devices found - Click scan</option>
          {:else}
            {#each bluetoothDevices as device}
              <option value={device.address}>{getDeviceDisplayName(device)}</option>
            {/each}
          {/if}
        </Select>
        <Button 
          variant="outline" 
          size="icon"
          on:click={scanForDevices}
          disabled={isScanning}
          class="h-10 w-10"
        >
          <RefreshCw class="h-4 w-4" />
        </Button>
      </div>
    </div>

    {#if !compact}
      <!-- Action Buttons -->
      <div class="grid grid-cols-2 gap-2">
        <Button 
          on:click={scanForDevices}
          disabled={isScanning}
          variant="outline"
        >
          {isScanning ? 'Scanning...' : 'Scan for Devices'}
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