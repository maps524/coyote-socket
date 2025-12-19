<script lang="ts">
  import { onMount, onDestroy, tick } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { listen, type UnlistenFn } from '@tauri-apps/api/event';
  import { getCurrentWindow, LogicalSize } from '@tauri-apps/api/window';
  import { Settings, HelpCircle, Zap, Info, Pause, Play } from 'lucide-svelte';
  import Tooltip from './lib/components/ui/Tooltip.svelte';
  import ConnectionPanel from './lib/components/ConnectionPanel.svelte';
  import BluetoothPanel, { type BluetoothPanelState, type BluetoothDevice } from './lib/components/BluetoothPanel.svelte';
  import InputStatusPill from './lib/components/InputStatusPill.svelte';
  import OutputStatusPill from './lib/components/OutputStatusPill.svelte';
  import ChannelControl from './lib/components/ChannelControl.svelte';
  import InputMonitor from './lib/components/InputMonitor.svelte';
  import Toggle from './lib/components/ui/Toggle.svelte';
  import Dialog from './lib/components/ui/Dialog.svelte';
  import Button from './lib/components/ui/Button.svelte';
  import TabsClassic from './lib/components/ui/TabsClassic.svelte';
  import Select from './lib/components/ui/Select.svelte';
  import LogsPanel from './lib/components/LogsPanel.svelte';
  import GeneralTab from './lib/components/settings/GeneralTab.svelte';
  import ButtplugTab from './lib/components/settings/ButtplugTab.svelte';
  import { outputOptions, connectionStatus, PROCESSING_ENGINES, type ProcessingEngine } from './lib/stores/connection.js';
  import { channelA, channelB } from './lib/stores/channels.js';
  import { generalSettings } from './lib/stores/generalSettings.js';
  import { startInputTracking, stopInputTracking } from './lib/stores/inputPosition.js';
  import {
    startStateSync,
    stopStateSync,
    getFullState,
    refreshConnectionStatus,
    connectionState,
    updateBatteryLevel,
    type FullAppState
  } from './lib/stores/stateSync.js';
  import { currentInputSource } from './lib/stores/inputSource.js';
  import { presetSelectionStore, setSelectedPreset, clearSelectedPreset } from './lib/stores/presetSelection.js';
  import type { AppSettings, ChannelPreset, ChannelSettings, PresetEcosystem } from './lib/types/settings';
  import { settingsToButtplugLinks } from './lib/types/settings';
  import type { NoInputBehavior } from './lib/types/modulation';
  import { coyoteService } from './lib/services/CoyoteService.js';
  import { Plus, Save, X } from 'lucide-svelte';

  let settingsOpen = false;
  let helpOpen = false;
  let outputPaused = false;

  // Connection state is now derived from the connectionState store (backend is source of truth)
  // These reactive declarations automatically update when the store changes
  $: inputConnected = $connectionState.websocket_running;
  $: outputConnected = $connectionState.bluetooth_connected;
  $: batteryLevel = $connectionState.battery_level;

  let settingsTab = 'general';
  let batteryPollInterval: ReturnType<typeof setInterval> | null = null;

  // Debounce rates (ms)
  const UPDATE_DEBOUNCE = 50;   // Backend state updates (real-time responsiveness)
  const SAVE_DEBOUNCE = 500;    // File persistence (reduce disk IO)

  // Update timers for backend state (50ms debounce)
  let outputUpdateTimer: ReturnType<typeof setTimeout> | null = null;
  let channelAUpdateTimer: ReturnType<typeof setTimeout> | null = null;
  let channelBUpdateTimer: ReturnType<typeof setTimeout> | null = null;

  // Save timers for file persistence (500ms debounce)
  let connectionSaveTimer: ReturnType<typeof setTimeout> | null = null;
  let bluetoothSaveTimer: ReturnType<typeof setTimeout> | null = null;
  let outputSaveTimer: ReturnType<typeof setTimeout> | null = null;
  let channelASaveTimer: ReturnType<typeof setTimeout> | null = null;
  let channelBSaveTimer: ReturnType<typeof setTimeout> | null = null;
  let shortcutsSaveTimer: ReturnType<typeof setTimeout> | null = null;

  // Keyboard shortcuts configuration
  let shortcuts = {
    channelAFreqUp: 'q',
    channelAFreqDown: 'a',
    channelAIntUp: 'r',
    channelAIntDown: 'f',
    channelAFreqBalUp: 'w',
    channelAFreqBalDown: 's',
    channelAIntBalUp: 'e',
    channelAIntBalDown: 'd',
    channelBFreqUp: '[',
    channelBFreqDown: "'",
    channelBIntUp: 'i',
    channelBIntDown: 'k',
    channelBFreqBalUp: 'p',
    channelBFreqBalDown: ';',
    channelBIntBalUp: 'o',
    channelBIntBalDown: 'l',
    help: '?',
    settings: ',',
    toggleOutputPause: ' '  // Space bar
  };

  // Connection settings
  let websocketPort = 12346;
  let selectedInterface = 0;
  let autoScan = true;
  let autoOpen = true;
  let autoConnect = true;  // Auto-connect to Coyote when found
  let settingsLoaded = false;  // Flag to prevent auto-save before settings are loaded
  let hmrReloading = true;  // Start true - prevents reactive sync until after initial load completes

  // savedSelectedDevice is a user preference (last device they connected to)
  let savedSelectedDevice = '';
  let inputPill: InputStatusPill;
  let outputPill: OutputStatusPill;
  let bluetoothPanel: BluetoothPanel;

  // Preset management
  let presets: ChannelPreset[] = [];
  let isAddingPreset = false;
  let newPresetName = '';
  let presetDirty = false;
  let lastSavedPresetState: { channelA: ChannelSettings, channelB: ChannelSettings } | null = null;

  // Event listener unsubscribe functions
  let unlistenOutputPause: UnlistenFn | null = null;

  // Derive current ecosystem from input source
  $: currentEcosystem = ($currentInputSource === 'tcode' || $currentInputSource === 'buttplug')
    ? $currentInputSource as PresetEcosystem
    : 'tcode' as PresetEcosystem;  // Default to tcode when no input

  // Get selected preset name from store based on current ecosystem
  $: selectedPresetName = $presetSelectionStore[currentEcosystem];

  // Filter presets based on current ecosystem
  $: filteredPresets = presets.filter(p => p.ecosystem === currentEcosystem);

  // Discovered Bluetooth devices come from the backend (not persisted to settings)
  $: bluetoothDevicesForComponents = $connectionState.discovered_devices.map(d => ({
    address: d.address,
    name: d.name ?? undefined,
    rssi: d.rssi ?? undefined
  }));

  onMount(async () => {
    console.log('CoyoteSocket application starting...');

    // FIRST: Start state sync and get live connection state immediately
    // This ensures connection status is shown without delay after HMR
    await startStateSync();
    try {
      const liveState = await getFullState();
      console.log('[StateSync] Live state from backend:', liveState);
      // Connection state is automatically updated by getFullState()
    } catch (e) {
      console.error('[StateSync] Failed to get live state:', e);
    }

    // Load saved settings from backend
    try {
      const settings = await invoke<AppSettings>('get_app_settings');
      console.log('[Settings] Loaded from backend:', settings);

      // Apply connection settings
      websocketPort = settings.connection.websocketPort ?? 12346;
      autoOpen = settings.connection.autoOpen;

      // Apply general settings (including showTCodeMonitor from connection settings for backwards compatibility)
      $generalSettings = {
        noInputBehavior: (settings.general?.noInputBehavior ?? 'hold') as NoInputBehavior,
        noInputDecayMs: settings.general?.noInputDecayMs ?? 1000,
        updateRateMs: settings.general?.updateRateMs ?? 50,
        saveRateMs: settings.general?.saveRateMs ?? 500,
        showTCodeMonitor: settings.connection.showTcodeMonitor,
        processingEngine: (settings.output.processingEngine as ProcessingEngine) ?? 'v1'
      };

      // Apply bluetooth settings (discovered devices come from backend, not settings)
      selectedInterface = settings.bluetooth.selectedInterface;
      autoScan = settings.bluetooth.autoScan;
      autoConnect = settings.bluetooth.autoConnect;
      savedSelectedDevice = settings.bluetooth.lastDevice || '';

      // Apply output settings
      $outputOptions = {
        processingEngine: (settings.output.processingEngine as ProcessingEngine) ?? 'v1'
      };

      // Apply channel A settings with new ParameterSource format
      const freqA = settings.channelA.frequencySource;
      const freqBalA = settings.channelA.frequencyBalanceSource;
      const intBalA = settings.channelA.intensityBalanceSource;
      const intA = settings.channelA.intensitySource;

      $channelA = {
        frequency: freqA.type === 'static' ? freqA.staticValue : 100,
        frequencyBalance: freqBalA.type === 'static' ? freqBalA.staticValue : 128,
        intensityBalance: intBalA.type === 'static' ? intBalA.staticValue : 128,
        period: Math.round(1000 / (freqA.type === 'static' ? freqA.staticValue : 100)),
        rangeMin: intA.rangeMin,
        rangeMax: intA.rangeMax,
        frequencySource: { ...freqA, curve: freqA.curve as any, buttplugLinks: settingsToButtplugLinks(freqA.buttplugLinks) },
        frequencyBalanceSource: { ...freqBalA, curve: freqBalA.curve as any, buttplugLinks: settingsToButtplugLinks(freqBalA.buttplugLinks) },
        intensityBalanceSource: { ...intBalA, curve: intBalA.curve as any, buttplugLinks: settingsToButtplugLinks(intBalA.buttplugLinks) },
        intensitySource: { ...intA, curve: intA.curve as any, buttplugLinks: settingsToButtplugLinks(intA.buttplugLinks) }
      };

      // Apply channel B settings with new ParameterSource format
      const freqB = settings.channelB.frequencySource;
      const freqBalB = settings.channelB.frequencyBalanceSource;
      const intBalB = settings.channelB.intensityBalanceSource;
      const intB = settings.channelB.intensitySource;

      $channelB = {
        frequency: freqB.type === 'static' ? freqB.staticValue : 100,
        frequencyBalance: freqBalB.type === 'static' ? freqBalB.staticValue : 128,
        intensityBalance: intBalB.type === 'static' ? intBalB.staticValue : 128,
        period: Math.round(1000 / (freqB.type === 'static' ? freqB.staticValue : 100)),
        rangeMin: intB.rangeMin,
        rangeMax: intB.rangeMax,
        frequencySource: { ...freqB, curve: freqB.curve as any, buttplugLinks: settingsToButtplugLinks(freqB.buttplugLinks) },
        frequencyBalanceSource: { ...freqBalB, curve: freqBalB.curve as any, buttplugLinks: settingsToButtplugLinks(freqBalB.buttplugLinks) },
        intensityBalanceSource: { ...intBalB, curve: intBalB.curve as any, buttplugLinks: settingsToButtplugLinks(intBalB.buttplugLinks) },
        intensitySource: { ...intB, curve: intB.curve as any, buttplugLinks: settingsToButtplugLinks(intB.buttplugLinks) }
      };

      // Apply keyboard shortcuts
      shortcuts = { ...settings.shortcuts };

      // Load output paused state from backend
      try {
        outputPaused = await invoke<boolean>('get_output_paused');
        console.log('[Settings] Output paused state:', outputPaused);
      } catch (e) {
        console.error('[Settings] Failed to load output paused state:', e);
        outputPaused = false;
      }

      // Listen for output pause state changes from backend
      unlistenOutputPause = await listen<{ paused: boolean }>('output-pause-changed', (event) => {
        outputPaused = event.payload.paused;
        console.log('[Event] Output pause changed:', outputPaused);
      });

    } catch (error) {
      console.error('[Settings] Failed to load from backend:', error);
      // Continue with defaults
    }

    // Load presets
    try {
      presets = await invoke<ChannelPreset[]>('get_presets');
      console.log('[Presets] Loaded', presets.length, 'presets');

      // If preset was restored from sessionStorage, initialize lastSavedPresetState
      // with the ORIGINAL preset values (not current channel values) for dirty tracking
      const restoredPresetName = $presetSelectionStore[currentEcosystem];
      if (restoredPresetName) {
        const preset = presets.find(p => p.name === restoredPresetName && p.ecosystem === currentEcosystem);
        if (preset) {
          // Use the preset's stored values as the baseline for dirty tracking
          lastSavedPresetState = { channelA: preset.channelA, channelB: preset.channelB };
          console.log('[Presets] Restored selection for', currentEcosystem, ':', restoredPresetName);
        } else {
          // Preset no longer exists, clear the selection
          clearSelectedPreset(currentEcosystem);
          console.log('[Presets] Previously selected preset no longer exists');
        }
      }
    } catch (error) {
      console.error('[Presets] Failed to load:', error);
      presets = [];
    }

    // Mark settings as loaded to enable auto-save
    console.log('[Settings] After loading - channelA:', $channelA);
    console.log('[Settings] After loading - generalSettings:', $generalSettings);
    settingsLoaded = true;

    // IMPORTANT: Wait for Svelte to process the store updates BEFORE enabling reactive sync
    // This prevents the loaded values from being pushed right back to the backend
    await tick();

    // Now reset HMR flag - reactive statements can sync user changes going forward
    hmrReloading = false;
    console.log('[Settings] settingsLoaded set to true, hmrReloading reset after tick');

    // Start input position tracking for range slider indicators
    startInputTracking();

    // Set initial window size based on T-Code monitor setting
    const initialHeight = $generalSettings.showTCodeMonitor ? WINDOW_HEIGHT_WITH_MONITOR : WINDOW_HEIGHT_COMPACT;
    const appWindow = getCurrentWindow();
    appWindow.setMinSize(new LogicalSize(600, initialHeight)).catch(() => {});
    appWindow.setSize(new LogicalSize(600, initialHeight)).catch(() => {});
    lastTCodeMonitorState = $generalSettings.showTCodeMonitor;

    // Auto-open input connection if enabled AND not already connected
    // (Skip if WebSocket is already running - e.g., after HMR refresh)
    if (autoOpen && !inputConnected) {
      console.log('Auto-opening WebSocket server...', { websocketPort });
      // Small delay to ensure everything is initialized
      setTimeout(async () => {
        try {
          const result = await invoke<string>('start_websocket_server', { port: websocketPort });
          console.log('Auto-open result:', result);
          // Connection state is updated via backend event (connection-changed)
        } catch (error) {
          console.error('Auto-open failed:', error);
        }
      }, 500);
    } else if (autoOpen && inputConnected) {
      console.log('[HMR] WebSocket already running, skipping auto-open');
    }

    // Auto-scan and auto-connect for Bluetooth output
    // (Skip if already connected - e.g., after HMR refresh)
    if (autoScan && !outputConnected) {
      console.log('Auto-scanning for Bluetooth devices...');
      // Delay to let the app initialize
      setTimeout(async () => {
        await scanAndConnect();
      }, 1000);
    } else if (autoScan && outputConnected) {
      console.log('[HMR] Bluetooth already connected, skipping auto-scan');
    }

    // Close splash screen and show main window
    // Small delay to ensure UI is fully rendered
    setTimeout(async () => {
      try {
        await invoke('close_splashscreen');
        console.log('Splash screen closed, main window shown');
      } catch (error) {
        console.error('Failed to close splash screen:', error);
      }
    }, 500);
  });

  // Scan for Bluetooth devices and optionally auto-connect
  async function scanAndConnect() {
    try {
      console.log('Scanning for Coyote devices on adapter:', selectedInterface);
      const devices = await invoke<BluetoothDevice[]>('scan_bluetooth_devices', {
        adapterIndex: Number(selectedInterface) || 0
      });

      console.log('Found devices:', devices);

      // Refresh connection state to get the updated discovered devices list from backend
      await refreshConnectionStatus();

      if (devices.length > 0) {
        // Find a Coyote device
        const coyoteDevice = devices.find(d =>
          d.name?.includes('COYOTE') ||
          d.name?.includes('DG-LAB') ||
          d.name?.includes('47L')
        );

        if (coyoteDevice) {
          savedSelectedDevice = coyoteDevice.address;
          console.log('Found Coyote device:', coyoteDevice);

          // Auto-connect if enabled
          if (autoConnect && !outputConnected) {
            console.log('Auto-connecting to Coyote device...');
            try {
              const result = await invoke<string>('connect_bluetooth_device', {
                adapterIndex: Number(selectedInterface) || 0,
                address: coyoteDevice.address
              });
              console.log('Auto-connect result:', result);
              // Connection state is updated via backend event (connection-changed)
            } catch (connectError) {
              console.error('Auto-connect failed:', connectError);
            }
          }
        } else {
          console.log('No Coyote device found in scan results');
        }
      } else {
        console.log('No Bluetooth devices found');
      }
    } catch (error) {
      console.error('Auto-scan failed:', error);
    }
  }

  onDestroy(() => {
    // Set HMR flag first to prevent reactive statements from syncing during reload
    hmrReloading = true;

    coyoteService.destroy();
    // Stop input position tracking
    stopInputTracking();
    // Stop state sync event listeners
    stopStateSync();
    // Clear output pause event listener
    if (unlistenOutputPause) unlistenOutputPause();
    // Clear all update timers (50ms)
    if (outputUpdateTimer) clearTimeout(outputUpdateTimer);
    if (channelAUpdateTimer) clearTimeout(channelAUpdateTimer);
    if (channelBUpdateTimer) clearTimeout(channelBUpdateTimer);
    // Clear all save timers (500ms)
    if (connectionSaveTimer) clearTimeout(connectionSaveTimer);
    if (generalSaveTimer) clearTimeout(generalSaveTimer);
    if (bluetoothSaveTimer) clearTimeout(bluetoothSaveTimer);
    if (outputSaveTimer) clearTimeout(outputSaveTimer);
    if (channelASaveTimer) clearTimeout(channelASaveTimer);
    if (channelBSaveTimer) clearTimeout(channelBSaveTimer);
    if (shortcutsSaveTimer) clearTimeout(shortcutsSaveTimer);
    if (batteryPollInterval) {
      clearInterval(batteryPollInterval);
    }
  });

  // Poll battery level when output connection changes
  $: {
    if (outputConnected && !batteryPollInterval) {
      // Initial read
      pollBattery();
      // Poll every 30 seconds
      batteryPollInterval = setInterval(pollBattery, 30000);
    } else if (!outputConnected && batteryPollInterval) {
      updateBatteryLevel(null);
      clearInterval(batteryPollInterval);
      batteryPollInterval = null;
    }
  }

  async function pollBattery() {
    try {
      const level = await invoke<number>('get_battery_level');
      if (level > 0) {
        updateBatteryLevel(level);
      }
    } catch (e) {
      console.error('Failed to poll battery:', e);
    }
  }

  // Window height constants
  const WINDOW_HEIGHT_COMPACT = 425;
  const WINDOW_HEIGHT_WITH_MONITOR = 600;

  // Resize window when T-Code monitor is toggled
  let lastTCodeMonitorState: boolean | null = null;

  async function resizeWindowForMonitor(monitorEnabled: boolean) {
    const targetHeight = monitorEnabled ? WINDOW_HEIGHT_WITH_MONITOR : WINDOW_HEIGHT_COMPACT;
    const appWindow = getCurrentWindow();

    try {
      await appWindow.setMinSize(new LogicalSize(600, targetHeight));
      const currentSize = await appWindow.innerSize();

      if (monitorEnabled && currentSize.height < WINDOW_HEIGHT_WITH_MONITOR) {
        await appWindow.setSize(new LogicalSize(currentSize.width, WINDOW_HEIGHT_WITH_MONITOR));
      } else if (!monitorEnabled) {
        await appWindow.setSize(new LogicalSize(currentSize.width, WINDOW_HEIGHT_COMPACT));
      }
    } catch (e) {
      console.error('Failed to resize window:', e);
    }
  }

  // Watch for T-Code monitor toggle changes
  $: {
    if (lastTCodeMonitorState !== null && lastTCodeMonitorState !== $generalSettings.showTCodeMonitor) {
      resizeWindowForMonitor($generalSettings.showTCodeMonitor);
    }
    lastTCodeMonitorState = $generalSettings.showTCodeMonitor;
  }

  // Debounced save for connection settings
  $: if (settingsLoaded && !hmrReloading) {
    const _trackConnectionChanges = [websocketPort, autoOpen];
    if (connectionSaveTimer) clearTimeout(connectionSaveTimer);
    connectionSaveTimer = setTimeout(() => {
      invoke('save_connection_settings', {
        websocketPort,
        autoOpen,
        showTcodeMonitor: $generalSettings.showTCodeMonitor
      }).catch((e) => console.error('[Settings] Failed to save connection settings:', e));
    }, 500);
  }

  // Debounced save for general settings
  let generalSaveTimer: ReturnType<typeof setTimeout> | null = null;
  $: if (settingsLoaded && !hmrReloading && $generalSettings) {
    if (generalSaveTimer) clearTimeout(generalSaveTimer);
    generalSaveTimer = setTimeout(() => {
      invoke('save_general_settings', {
        noInputBehavior: $generalSettings.noInputBehavior,
        noInputDecayMs: $generalSettings.noInputDecayMs,
        updateRateMs: $generalSettings.updateRateMs,
        saveRateMs: $generalSettings.saveRateMs,
        showTcodeMonitor: $generalSettings.showTCodeMonitor,
        processingEngine: $generalSettings.processingEngine
      }).catch((e) => console.error('[Settings] Failed to save general settings:', e));
    }, $generalSettings.saveRateMs ?? 500);
  }

  // Debounced save for bluetooth settings (user preferences only, not discovered devices)
  $: if (settingsLoaded && !hmrReloading) {
    const _trackBluetoothChanges = [selectedInterface, autoScan, autoConnect, savedSelectedDevice];
    if (bluetoothSaveTimer) clearTimeout(bluetoothSaveTimer);
    bluetoothSaveTimer = setTimeout(() => {
      const btState: BluetoothPanelState | undefined = bluetoothPanel?.getState?.();
      invoke('save_bluetooth_settings', {
        selectedInterface,
        autoScan,
        autoConnect,
        // Don't save discovered devices - they come from backend state
        savedDevices: [],
        lastDevice: btState?.selectedDevice || savedSelectedDevice || null
      }).catch((e) => console.error('[Settings] Failed to save bluetooth settings:', e));
    }, 500);
  }

  // Sync output options to backend (50ms) and save to file (500ms)
  // Note: interplay and chaseDelayMs are hardcoded until backend cleanup (see TODO.md)
  $: if ($generalSettings && settingsLoaded && !hmrReloading) {
    // Fast update to backend state
    if (outputUpdateTimer) clearTimeout(outputUpdateTimer);
    outputUpdateTimer = setTimeout(() => {
      invoke('update_output_options', {
        interplay: 'none',
        engine: $generalSettings.processingEngine ?? 'v1',
        chaseDelayMs: 100
      }).catch(() => {}); // Silently ignore update errors
    }, UPDATE_DEBOUNCE);

    // Slower save to file
    if (outputSaveTimer) clearTimeout(outputSaveTimer);
    outputSaveTimer = setTimeout(() => {
      invoke('save_output_settings', {
        channelInterplay: 'none',
        processingEngine: $generalSettings.processingEngine ?? 'v1',
        chaseDelayMs: 100
      }).catch((e) => console.error('[Settings] Failed to save output settings:', e));
    }, SAVE_DEBOUNCE);
  }

  // Sync channel A to backend (50ms) and save to file (500ms)
  $: if ($channelA && settingsLoaded && !hmrReloading) {
    // Fast update to backend state
    if (channelAUpdateTimer) clearTimeout(channelAUpdateTimer);
    channelAUpdateTimer = setTimeout(() => {
      invoke('update_channel_params', {
        channel: 'A',
        frequency: $channelA.frequency,
        freqBalance: $channelA.frequencyBalance,
        intBalance: $channelA.intensityBalance,
        rangeMin: $channelA.rangeMin,
        rangeMax: $channelA.rangeMax
      }).catch(() => {}); // Silently ignore update errors
    }, UPDATE_DEBOUNCE);

    // Slower save to file with full ParameterSource format
    if (channelASaveTimer) clearTimeout(channelASaveTimer);
    channelASaveTimer = setTimeout(() => {
      const channelSettings = {
        frequencySource: {
          type: $channelA.frequencySource?.type ?? 'static',
          staticValue: $channelA.frequencySource?.staticValue ?? $channelA.frequency,
          sourceAxis: $channelA.frequencySource?.sourceAxis ?? 'L0',
          rangeMin: $channelA.frequencySource?.rangeMin ?? 1,
          rangeMax: $channelA.frequencySource?.rangeMax ?? 200,
          curve: $channelA.frequencySource?.curve ?? 'linear',
          curveStrength: $channelA.frequencySource?.curveStrength ?? 2.0,
          buttplugLinks: $channelA.frequencySource?.buttplugLinks
        },
        frequencyBalanceSource: {
          type: $channelA.frequencyBalanceSource?.type ?? 'static',
          staticValue: $channelA.frequencyBalanceSource?.staticValue ?? $channelA.frequencyBalance,
          sourceAxis: $channelA.frequencyBalanceSource?.sourceAxis ?? 'L0',
          rangeMin: $channelA.frequencyBalanceSource?.rangeMin ?? 0,
          rangeMax: $channelA.frequencyBalanceSource?.rangeMax ?? 255,
          curve: $channelA.frequencyBalanceSource?.curve ?? 'linear',
          curveStrength: $channelA.frequencyBalanceSource?.curveStrength ?? 2.0,
          buttplugLinks: $channelA.frequencyBalanceSource?.buttplugLinks
        },
        intensityBalanceSource: {
          type: $channelA.intensityBalanceSource?.type ?? 'static',
          staticValue: $channelA.intensityBalanceSource?.staticValue ?? $channelA.intensityBalance,
          sourceAxis: $channelA.intensityBalanceSource?.sourceAxis ?? 'L0',
          rangeMin: $channelA.intensityBalanceSource?.rangeMin ?? 0,
          rangeMax: $channelA.intensityBalanceSource?.rangeMax ?? 255,
          curve: $channelA.intensityBalanceSource?.curve ?? 'linear',
          curveStrength: $channelA.intensityBalanceSource?.curveStrength ?? 2.0,
          buttplugLinks: $channelA.intensityBalanceSource?.buttplugLinks
        },
        intensitySource: {
          type: $channelA.intensitySource?.type ?? 'linked',
          staticValue: $channelA.intensitySource?.staticValue ?? 100,
          sourceAxis: $channelA.intensitySource?.sourceAxis ?? 'L0',
          rangeMin: $channelA.intensitySource?.rangeMin ?? $channelA.rangeMin,
          rangeMax: $channelA.intensitySource?.rangeMax ?? $channelA.rangeMax,
          curve: $channelA.intensitySource?.curve ?? 'linear',
          curveStrength: $channelA.intensitySource?.curveStrength ?? 2.0,
          buttplugLinks: $channelA.intensitySource?.buttplugLinks
        }
      };
      invoke('save_channel_settings', { channel: 'A', channelSettings })
        .catch((e) => console.error('[Settings] Failed to save channel A settings:', e));
    }, SAVE_DEBOUNCE);
  }

  // Sync channel B to backend (50ms) and save to file (500ms)
  $: if ($channelB && settingsLoaded && !hmrReloading) {
    // Fast update to backend state
    if (channelBUpdateTimer) clearTimeout(channelBUpdateTimer);
    channelBUpdateTimer = setTimeout(() => {
      invoke('update_channel_params', {
        channel: 'B',
        frequency: $channelB.frequency,
        freqBalance: $channelB.frequencyBalance,
        intBalance: $channelB.intensityBalance,
        rangeMin: $channelB.rangeMin,
        rangeMax: $channelB.rangeMax
      }).catch(() => {}); // Silently ignore update errors
    }, UPDATE_DEBOUNCE);

    // Slower save to file with full ParameterSource format
    if (channelBSaveTimer) clearTimeout(channelBSaveTimer);
    channelBSaveTimer = setTimeout(() => {
      const channelSettings = {
        frequencySource: {
          type: $channelB.frequencySource?.type ?? 'static',
          staticValue: $channelB.frequencySource?.staticValue ?? $channelB.frequency,
          sourceAxis: $channelB.frequencySource?.sourceAxis ?? 'R2',
          rangeMin: $channelB.frequencySource?.rangeMin ?? 1,
          rangeMax: $channelB.frequencySource?.rangeMax ?? 200,
          curve: $channelB.frequencySource?.curve ?? 'linear',
          curveStrength: $channelB.frequencySource?.curveStrength ?? 2.0,
          buttplugLinks: $channelB.frequencySource?.buttplugLinks
        },
        frequencyBalanceSource: {
          type: $channelB.frequencyBalanceSource?.type ?? 'static',
          staticValue: $channelB.frequencyBalanceSource?.staticValue ?? $channelB.frequencyBalance,
          sourceAxis: $channelB.frequencyBalanceSource?.sourceAxis ?? 'R2',
          rangeMin: $channelB.frequencyBalanceSource?.rangeMin ?? 0,
          rangeMax: $channelB.frequencyBalanceSource?.rangeMax ?? 255,
          curve: $channelB.frequencyBalanceSource?.curve ?? 'linear',
          curveStrength: $channelB.frequencyBalanceSource?.curveStrength ?? 2.0,
          buttplugLinks: $channelB.frequencyBalanceSource?.buttplugLinks
        },
        intensityBalanceSource: {
          type: $channelB.intensityBalanceSource?.type ?? 'static',
          staticValue: $channelB.intensityBalanceSource?.staticValue ?? $channelB.intensityBalance,
          sourceAxis: $channelB.intensityBalanceSource?.sourceAxis ?? 'R2',
          rangeMin: $channelB.intensityBalanceSource?.rangeMin ?? 0,
          rangeMax: $channelB.intensityBalanceSource?.rangeMax ?? 255,
          curve: $channelB.intensityBalanceSource?.curve ?? 'linear',
          curveStrength: $channelB.intensityBalanceSource?.curveStrength ?? 2.0,
          buttplugLinks: $channelB.intensityBalanceSource?.buttplugLinks
        },
        intensitySource: {
          type: $channelB.intensitySource?.type ?? 'linked',
          staticValue: $channelB.intensitySource?.staticValue ?? 100,
          sourceAxis: $channelB.intensitySource?.sourceAxis ?? 'R2',
          rangeMin: $channelB.intensitySource?.rangeMin ?? $channelB.rangeMin,
          rangeMax: $channelB.intensitySource?.rangeMax ?? $channelB.rangeMax,
          curve: $channelB.intensitySource?.curve ?? 'linear',
          curveStrength: $channelB.intensitySource?.curveStrength ?? 2.0,
          buttplugLinks: $channelB.intensitySource?.buttplugLinks
        }
      };
      invoke('save_channel_settings', { channel: 'B', channelSettings })
        .catch((e) => console.error('[Settings] Failed to save channel B settings:', e));
    }, 500);
  }

  // Save shortcuts when they change
  $: if (settingsLoaded && !hmrReloading && shortcuts) {
    if (shortcutsSaveTimer) clearTimeout(shortcutsSaveTimer);
    shortcutsSaveTimer = setTimeout(() => {
      invoke('save_shortcuts', {
        channelAFreqUp: shortcuts.channelAFreqUp,
        channelAFreqDown: shortcuts.channelAFreqDown,
        channelAIntUp: shortcuts.channelAIntUp,
        channelAIntDown: shortcuts.channelAIntDown,
        channelAFreqBalUp: shortcuts.channelAFreqBalUp,
        channelAFreqBalDown: shortcuts.channelAFreqBalDown,
        channelAIntBalUp: shortcuts.channelAIntBalUp,
        channelAIntBalDown: shortcuts.channelAIntBalDown,
        channelBFreqUp: shortcuts.channelBFreqUp,
        channelBFreqDown: shortcuts.channelBFreqDown,
        channelBIntUp: shortcuts.channelBIntUp,
        channelBIntDown: shortcuts.channelBIntDown,
        channelBFreqBalUp: shortcuts.channelBFreqBalUp,
        channelBFreqBalDown: shortcuts.channelBFreqBalDown,
        channelBIntBalUp: shortcuts.channelBIntBalUp,
        channelBIntBalDown: shortcuts.channelBIntBalDown,
        help: shortcuts.help,
        settingsKey: shortcuts.settings,
        toggleOutputPause: shortcuts.toggleOutputPause
      }).catch((e) => console.error('[Settings] Failed to save shortcuts:', e));
    }, 500);
  }

  async function saveSettings() {
    // Save is now handled by reactive statements, but we can manually trigger saves
    // and show feedback for the Save button
    try {
      // Get current Bluetooth state for selected device
      const btState: BluetoothPanelState | undefined = bluetoothPanel?.getState?.();
      savedSelectedDevice = btState?.selectedDevice || savedSelectedDevice;

      // The reactive statements will handle the actual saving
      // Just trigger a save of bluetooth settings (without devices - they're backend state)
      await invoke('save_bluetooth_settings', {
        selectedInterface,
        autoScan,
        autoConnect,
        savedDevices: [], // Devices come from backend, not settings
        lastDevice: savedSelectedDevice || null
      });

      console.log('[Settings] Settings saved via button');

      // Show success feedback
      const originalText = document.querySelector('.save-button')?.textContent;
      const button = document.querySelector('.save-button');
      if (button) {
        button.textContent = 'Saved!';
        setTimeout(() => {
          button.textContent = originalText || 'Save Settings';
        }, 2000);
      }
    } catch (e) {
      console.error('[Settings] Failed to save settings:', e);
    }
  }

  // Keyboard shortcuts
  function handleKeydown(e: KeyboardEvent) {
    // Don't handle shortcuts when typing in inputs
    if (e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement) return;

    // Channel A controls
    if (e.key === shortcuts.channelAFreqDown) adjustFrequency('A', 'down');
    if (e.key === shortcuts.channelAFreqUp) adjustFrequency('A', 'up');
    if (e.key === shortcuts.channelAIntDown) adjustIntensity('A', -5);
    if (e.key === shortcuts.channelAIntUp) adjustIntensity('A', 5);
    if (e.key === shortcuts.channelAFreqBalDown) adjustBalance('A', 'frequency', -5);
    if (e.key === shortcuts.channelAFreqBalUp) adjustBalance('A', 'frequency', 5);
    if (e.key === shortcuts.channelAIntBalDown) adjustBalance('A', 'intensity', -5);
    if (e.key === shortcuts.channelAIntBalUp) adjustBalance('A', 'intensity', 5);

    // Channel B controls
    if (e.key === shortcuts.channelBFreqDown) adjustFrequency('B', 'down');
    if (e.key === shortcuts.channelBFreqUp) adjustFrequency('B', 'up');
    if (e.key === shortcuts.channelBIntDown) adjustIntensity('B', -5);
    if (e.key === shortcuts.channelBIntUp) adjustIntensity('B', 5);
    if (e.key === shortcuts.channelBFreqBalDown) adjustBalance('B', 'frequency', -5);
    if (e.key === shortcuts.channelBFreqBalUp) adjustBalance('B', 'frequency', 5);
    if (e.key === shortcuts.channelBIntBalDown) adjustBalance('B', 'intensity', -5);
    if (e.key === shortcuts.channelBIntBalUp) adjustBalance('B', 'intensity', 5);

    // Settings/Help
    if (e.key === shortcuts.help && e.shiftKey) helpOpen = true;
    if (e.key === shortcuts.settings) settingsOpen = true;

    // Output pause toggle
    if (e.key === shortcuts.toggleOutputPause) {
      e.preventDefault(); // Prevent space from scrolling
      toggleOutputPause();
    }
  }

  async function toggleOutputPause() {
    try {
      const newState = await invoke<boolean>('toggle_output_paused');
      outputPaused = newState;
    } catch (error) {
      console.error('Failed to toggle output pause:', error);
    }
  }

  function adjustFrequency(channel: string, direction: 'up' | 'down') {
    const store = channel === 'A' ? channelA : channelB;
    store.update(s => {
      const source = s.frequencySource;

      // If linked mode, adjust the range instead of static value
      if (source?.type === 'linked') {
        const currentMin = source.rangeMin ?? 1;
        const currentMax = source.rangeMax ?? 200;
        const rangeSize = currentMax - currentMin;
        const delta = direction === 'up' ? 5 : -5;

        let newMin = currentMin + delta;
        let newMax = currentMax + delta;

        // Clamp to boundaries while preserving range size
        if (newMin < 1) {
          newMin = 1;
          newMax = 1 + rangeSize;
        }
        if (newMax > 200) {
          newMax = 200;
          newMin = Math.max(1, 200 - rangeSize);
        }

        return {
          ...s,
          frequencySource: { ...source, rangeMin: newMin, rangeMax: newMax }
        };
      }

      // Static mode: adjust via period
      const currentPeriod = Math.round(1000 / s.frequency);
      // Decrease period = increase frequency, increase period = decrease frequency
      const newPeriod = direction === 'up' ? currentPeriod - 1 : currentPeriod + 1;
      // Clamp period to valid range (5ms = 200Hz, 1000ms = 1Hz)
      const clampedPeriod = Math.max(5, Math.min(1000, newPeriod));
      const newFrequency = 1000 / clampedPeriod;

      // Update both top-level and source
      const updatedSource = source
        ? { ...source, staticValue: newFrequency }
        : undefined;

      return { ...s, frequency: newFrequency, frequencySource: updatedSource };
    });
  }

  function adjustIntensity(channel: string, delta: number) {
    const store = channel === 'A' ? channelA : channelB;
    store.update(s => {
      const source = s.intensitySource;

      // If static mode, adjust the static value
      if (source?.type === 'static') {
        const currentValue = source.staticValue ?? 100;
        const newValue = Math.max(0, Math.min(200, currentValue + delta));

        return {
          ...s,
          intensitySource: { ...source, staticValue: newValue }
        };
      }

      // Linked mode: adjust the range (shift min/max together)
      const currentMin = source?.rangeMin ?? s.rangeMin;
      const currentMax = source?.rangeMax ?? s.rangeMax;
      const rangeSize = currentMax - currentMin;

      let newMin = currentMin + delta;
      let newMax = currentMax + delta;

      // Clamp to boundaries while preserving range size
      if (newMin < 0) {
        newMin = 0;
        newMax = rangeSize;
      }
      if (newMax > 200) {
        newMax = 200;
        newMin = 200 - rangeSize;
      }

      // Update both top-level and intensitySource
      const updatedIntensitySource = source
        ? { ...source, rangeMin: newMin, rangeMax: newMax }
        : undefined;

      return {
        ...s,
        rangeMin: newMin,
        rangeMax: newMax,
        intensitySource: updatedIntensitySource
      };
    });
  }

  function adjustBalance(channel: string, type: 'frequency' | 'intensity', delta: number) {
    const store = channel === 'A' ? channelA : channelB;
    const field = type === 'frequency' ? 'frequencyBalance' : 'intensityBalance';
    const sourceField = type === 'frequency' ? 'frequencyBalanceSource' : 'intensityBalanceSource';

    store.update(s => {
      const source = s[sourceField];

      // If linked mode, adjust the range instead of static value
      if (source?.type === 'linked') {
        const currentMin = source.rangeMin ?? 0;
        const currentMax = source.rangeMax ?? 255;
        const rangeSize = currentMax - currentMin;

        let newMin = currentMin + delta;
        let newMax = currentMax + delta;

        // Clamp to boundaries while preserving range size
        if (newMin < 0) {
          newMin = 0;
          newMax = rangeSize;
        }
        if (newMax > 255) {
          newMax = 255;
          newMin = Math.max(0, 255 - rangeSize);
        }

        return {
          ...s,
          [sourceField]: { ...source, rangeMin: newMin, rangeMax: newMax }
        };
      }

      // Static mode: adjust the direct value
      const newValue = Math.max(0, Math.min(255, s[field] + delta));

      // Update both top-level and source
      const updatedSource = source
        ? { ...source, staticValue: newValue }
        : undefined;

      return {
        ...s,
        [field]: newValue,
        [sourceField]: updatedSource
      };
    });
  }

  // Preset management functions
  function getCurrentChannelSettings(): { channelA: ChannelSettings, channelB: ChannelSettings } {
    return {
      channelA: {
        frequencySource: {
          type: $channelA.frequencySource?.type ?? 'static',
          staticValue: $channelA.frequencySource?.staticValue ?? $channelA.frequency,
          sourceAxis: $channelA.frequencySource?.sourceAxis ?? 'L0',
          rangeMin: $channelA.frequencySource?.rangeMin ?? 1,
          rangeMax: $channelA.frequencySource?.rangeMax ?? 200,
          curve: $channelA.frequencySource?.curve ?? 'linear',
          curveStrength: $channelA.frequencySource?.curveStrength ?? 2.0,
          midpoint: $channelA.frequencySource?.midpoint
        },
        frequencyBalanceSource: {
          type: $channelA.frequencyBalanceSource?.type ?? 'static',
          staticValue: $channelA.frequencyBalanceSource?.staticValue ?? $channelA.frequencyBalance,
          sourceAxis: $channelA.frequencyBalanceSource?.sourceAxis ?? 'L0',
          rangeMin: $channelA.frequencyBalanceSource?.rangeMin ?? 0,
          rangeMax: $channelA.frequencyBalanceSource?.rangeMax ?? 255,
          curve: $channelA.frequencyBalanceSource?.curve ?? 'linear',
          curveStrength: $channelA.frequencyBalanceSource?.curveStrength ?? 2.0,
          midpoint: $channelA.frequencyBalanceSource?.midpoint
        },
        intensityBalanceSource: {
          type: $channelA.intensityBalanceSource?.type ?? 'static',
          staticValue: $channelA.intensityBalanceSource?.staticValue ?? $channelA.intensityBalance,
          sourceAxis: $channelA.intensityBalanceSource?.sourceAxis ?? 'L0',
          rangeMin: $channelA.intensityBalanceSource?.rangeMin ?? 0,
          rangeMax: $channelA.intensityBalanceSource?.rangeMax ?? 255,
          curve: $channelA.intensityBalanceSource?.curve ?? 'linear',
          curveStrength: $channelA.intensityBalanceSource?.curveStrength ?? 2.0,
          midpoint: $channelA.intensityBalanceSource?.midpoint
        },
        intensitySource: {
          type: $channelA.intensitySource?.type ?? 'linked',
          staticValue: $channelA.intensitySource?.staticValue ?? 100,
          sourceAxis: $channelA.intensitySource?.sourceAxis ?? 'L0',
          rangeMin: $channelA.intensitySource?.rangeMin ?? $channelA.rangeMin,
          rangeMax: $channelA.intensitySource?.rangeMax ?? $channelA.rangeMax,
          curve: $channelA.intensitySource?.curve ?? 'linear',
          curveStrength: $channelA.intensitySource?.curveStrength ?? 2.0,
          midpoint: $channelA.intensitySource?.midpoint
        }
      },
      channelB: {
        frequencySource: {
          type: $channelB.frequencySource?.type ?? 'static',
          staticValue: $channelB.frequencySource?.staticValue ?? $channelB.frequency,
          sourceAxis: $channelB.frequencySource?.sourceAxis ?? 'R2',
          rangeMin: $channelB.frequencySource?.rangeMin ?? 1,
          rangeMax: $channelB.frequencySource?.rangeMax ?? 200,
          curve: $channelB.frequencySource?.curve ?? 'linear',
          curveStrength: $channelB.frequencySource?.curveStrength ?? 2.0,
          midpoint: $channelB.frequencySource?.midpoint
        },
        frequencyBalanceSource: {
          type: $channelB.frequencyBalanceSource?.type ?? 'static',
          staticValue: $channelB.frequencyBalanceSource?.staticValue ?? $channelB.frequencyBalance,
          sourceAxis: $channelB.frequencyBalanceSource?.sourceAxis ?? 'R2',
          rangeMin: $channelB.frequencyBalanceSource?.rangeMin ?? 0,
          rangeMax: $channelB.frequencyBalanceSource?.rangeMax ?? 255,
          curve: $channelB.frequencyBalanceSource?.curve ?? 'linear',
          curveStrength: $channelB.frequencyBalanceSource?.curveStrength ?? 2.0,
          midpoint: $channelB.frequencyBalanceSource?.midpoint
        },
        intensityBalanceSource: {
          type: $channelB.intensityBalanceSource?.type ?? 'static',
          staticValue: $channelB.intensityBalanceSource?.staticValue ?? $channelB.intensityBalance,
          sourceAxis: $channelB.intensityBalanceSource?.sourceAxis ?? 'R2',
          rangeMin: $channelB.intensityBalanceSource?.rangeMin ?? 0,
          rangeMax: $channelB.intensityBalanceSource?.rangeMax ?? 255,
          curve: $channelB.intensityBalanceSource?.curve ?? 'linear',
          curveStrength: $channelB.intensityBalanceSource?.curveStrength ?? 2.0,
          midpoint: $channelB.intensityBalanceSource?.midpoint
        },
        intensitySource: {
          type: $channelB.intensitySource?.type ?? 'linked',
          staticValue: $channelB.intensitySource?.staticValue ?? 100,
          sourceAxis: $channelB.intensitySource?.sourceAxis ?? 'R2',
          rangeMin: $channelB.intensitySource?.rangeMin ?? $channelB.rangeMin,
          rangeMax: $channelB.intensitySource?.rangeMax ?? $channelB.rangeMax,
          curve: $channelB.intensitySource?.curve ?? 'linear',
          curveStrength: $channelB.intensitySource?.curveStrength ?? 2.0,
          midpoint: $channelB.intensitySource?.midpoint
        }
      }
    };
  }

  function applyPreset(preset: ChannelPreset) {
    // Apply channel A
    const aFreq = preset.channelA.frequencySource;
    const aFreqBal = preset.channelA.frequencyBalanceSource;
    const aIntBal = preset.channelA.intensityBalanceSource;
    const aInt = preset.channelA.intensitySource;

    $channelA = {
      frequency: aFreq.type === 'static' ? aFreq.staticValue : 100,
      frequencyBalance: aFreqBal.type === 'static' ? aFreqBal.staticValue : 128,
      intensityBalance: aIntBal.type === 'static' ? aIntBal.staticValue : 128,
      period: Math.round(1000 / (aFreq.type === 'static' ? aFreq.staticValue : 100)),
      rangeMin: aInt.rangeMin,
      rangeMax: aInt.rangeMax,
      frequencySource: { ...aFreq, curve: aFreq.curve as any, buttplugLinks: settingsToButtplugLinks(aFreq.buttplugLinks) },
      frequencyBalanceSource: { ...aFreqBal, curve: aFreqBal.curve as any, buttplugLinks: settingsToButtplugLinks(aFreqBal.buttplugLinks) },
      intensityBalanceSource: { ...aIntBal, curve: aIntBal.curve as any, buttplugLinks: settingsToButtplugLinks(aIntBal.buttplugLinks) },
      intensitySource: { ...aInt, curve: aInt.curve as any, buttplugLinks: settingsToButtplugLinks(aInt.buttplugLinks) }
    };

    // Apply channel B
    const bFreq = preset.channelB.frequencySource;
    const bFreqBal = preset.channelB.frequencyBalanceSource;
    const bIntBal = preset.channelB.intensityBalanceSource;
    const bInt = preset.channelB.intensitySource;

    $channelB = {
      frequency: bFreq.type === 'static' ? bFreq.staticValue : 100,
      frequencyBalance: bFreqBal.type === 'static' ? bFreqBal.staticValue : 128,
      intensityBalance: bIntBal.type === 'static' ? bIntBal.staticValue : 128,
      period: Math.round(1000 / (bFreq.type === 'static' ? bFreq.staticValue : 100)),
      rangeMin: bInt.rangeMin,
      rangeMax: bInt.rangeMax,
      frequencySource: { ...bFreq, curve: bFreq.curve as any, buttplugLinks: settingsToButtplugLinks(bFreq.buttplugLinks) },
      frequencyBalanceSource: { ...bFreqBal, curve: bFreqBal.curve as any, buttplugLinks: settingsToButtplugLinks(bFreqBal.buttplugLinks) },
      intensityBalanceSource: { ...bIntBal, curve: bIntBal.curve as any, buttplugLinks: settingsToButtplugLinks(bIntBal.buttplugLinks) },
      intensitySource: { ...bInt, curve: bInt.curve as any, buttplugLinks: settingsToButtplugLinks(bInt.buttplugLinks) }
    };

    // Store state for dirty tracking
    lastSavedPresetState = getCurrentChannelSettings();
    presetDirty = false;
  }

  async function handlePresetSelect(name: string) {
    if (!name) {
      clearSelectedPreset(currentEcosystem);
      lastSavedPresetState = null;
      presetDirty = false;
      return;
    }

    const preset = presets.find(p => p.name === name && p.ecosystem === currentEcosystem);
    if (preset) {
      setSelectedPreset(currentEcosystem, name);
      applyPreset(preset);
    }
  }

  function startAddingPreset() {
    isAddingPreset = true;
    newPresetName = '';
  }

  function cancelAddingPreset() {
    isAddingPreset = false;
    newPresetName = '';
  }

  async function saveNewPreset() {
    if (!newPresetName.trim()) return;

    const current = getCurrentChannelSettings();
    const preset: ChannelPreset = {
      name: newPresetName.trim(),
      ecosystem: currentEcosystem,
      channelA: current.channelA,
      channelB: current.channelB
    };

    try {
      await invoke('save_preset', { preset });
      presets = await invoke<ChannelPreset[]>('get_presets');
      setSelectedPreset(currentEcosystem, preset.name);
      lastSavedPresetState = current;
      presetDirty = false;
      isAddingPreset = false;
      newPresetName = '';
      console.log('[Presets] Saved new preset:', preset.name, 'for ecosystem:', currentEcosystem);
    } catch (error) {
      console.error('[Presets] Failed to save:', error);
    }
  }

  async function saveCurrentPreset() {
    if (!selectedPresetName) return;

    const current = getCurrentChannelSettings();
    const preset: ChannelPreset = {
      name: selectedPresetName,
      ecosystem: currentEcosystem,
      channelA: current.channelA,
      channelB: current.channelB
    };

    try {
      await invoke('save_preset', { preset });
      presets = await invoke<ChannelPreset[]>('get_presets');
      lastSavedPresetState = current;
      presetDirty = false;
      console.log('[Presets] Updated preset:', preset.name, 'for ecosystem:', currentEcosystem);
    } catch (error) {
      console.error('[Presets] Failed to save:', error);
    }
  }

  // Check if preset is dirty (channel values changed since loading preset)
  function checkPresetDirty(): boolean {
    if (!selectedPresetName || !lastSavedPresetState) return false;

    const current = getCurrentChannelSettings();

    // Deep compare channel settings
    const stringify = (obj: any) => JSON.stringify(obj);
    return stringify(current.channelA) !== stringify(lastSavedPresetState.channelA) ||
           stringify(current.channelB) !== stringify(lastSavedPresetState.channelB);
  }

  // Watch for channel changes to update dirty state
  // Include channel stores as dependencies so this re-runs when they change
  $: if (settingsLoaded && selectedPresetName && lastSavedPresetState && $channelA && $channelB) {
    presetDirty = checkPresetDirty();
  }

</script>

<svelte:window on:keydown={handleKeydown} />

<main class="h-screen w-full bg-background text-foreground overflow-hidden flex flex-col">
  <!-- Compact Header -->
  <header class="border-b border-border bg-card/50 backdrop-blur w-full">
    <div class="w-full px-4 py-3 max-w-[1920px] mx-auto">
      <div class="flex items-center justify-between">
        <div class="flex items-center gap-3">
          <Zap class="h-6 w-6 text-primary" />
          <h1 class="text-xl font-bold bg-gradient-to-r from-primary to-secondary bg-clip-text text-transparent">
            CoyoteSocket
          </h1>
        </div>

        <div class="flex items-center gap-2">
          <!-- Input Status Pill with Popover -->
          <InputStatusPill
            bind:this={inputPill}
            isConnected={inputConnected}
            bind:websocketPort
            bind:autoOpen
            showTCodeMonitor={$generalSettings.showTCodeMonitor}
            onConnectionChange={() => {/* Backend events handle state */}}
          />

          <!-- Output Status Pill with Popover -->
          <OutputStatusPill
            bind:this={outputPill}
            isConnected={outputConnected}
            {batteryLevel}
            bind:selectedInterface
            bind:autoScan
            bind:autoConnect
            savedDevices={bluetoothDevicesForComponents}
            savedSelectedDevice={savedSelectedDevice}
            onConnectionChange={() => {/* Backend events handle state */}}
          />

          <!-- Pause/Play Button -->
          <Tooltip content={outputPaused ? `Resume output <code>${shortcuts.toggleOutputPause === ' ' ? 'Space' : shortcuts.toggleOutputPause}</code>` : `Pause output <code>${shortcuts.toggleOutputPause === ' ' ? 'Space' : shortcuts.toggleOutputPause}</code>`} side="bottom">
            <button
              on:click={toggleOutputPause}
              class="flex items-center justify-center h-[26px] w-[26px] rounded-full transition-all
                     {outputPaused
                       ? 'bg-yellow-500/20 text-yellow-400 border border-yellow-500/30 hover:bg-yellow-500/30'
                       : 'bg-green-500/20 text-green-400 border border-green-500/30 hover:bg-green-500/30'}"
            >
              {#if outputPaused}
                <Pause class="h-3.5 w-3.5" />
              {:else}
                <Play class="h-3.5 w-3.5" />
              {/if}
            </button>
          </Tooltip>

          <Button
            variant="ghost"
            size="icon"
            on:click={() => settingsOpen = true}
            class="h-8 w-8"
          >
            <Settings class="h-4 w-4" />
          </Button>

          <Button
            variant="ghost"
            size="icon"
            on:click={() => helpOpen = true}
            class="h-8 w-8"
          >
            <HelpCircle class="h-4 w-4" />
          </Button>
        </div>
      </div>
    </div>
  </header>

  <!-- Main Content - Compact Layout -->
  <div class="flex-1 min-h-0 overflow-y-auto scrollbar-thin">
    <div class="px-4 py-4 max-w-[1920px] mx-auto">
    <div class="space-y-4">
      <!-- Output Options -->
      <div class="flex items-center gap-4">
          <!-- Preset Selector -->
          <div class="flex h-7 pr-1 rounded border border-border overflow-hidden bg-background/50">
            <span class="text-xs text-muted-foreground px-2 flex items-center border-r border-border bg-muted/30">Preset</span>
            <div class="flex flex-1">
              {#if isAddingPreset}
                <input
                  type="text"
                  bind:value={newPresetName}
                  placeholder="Preset name"
                  class="flex-1 py-1 px-2 text-xs bg-transparent border-none outline-none min-w-0 text-foreground placeholder:text-muted-foreground"
                  on:keydown={(e) => e.key === 'Enter' && saveNewPreset()}
                />
                <button
                  on:click={saveNewPreset}
                  disabled={!newPresetName.trim()}
                  class="w-7 flex items-center justify-center bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50 border-l border-border"
                  title="Save preset"
                >
                  <Save class="h-3.5 w-3.5" />
                </button>
                <button
                  on:click={cancelAddingPreset}
                  class="w-7 flex items-center justify-center hover:bg-background/50 border-l border-border"
                  title="Cancel"
                >
                  <X class="h-3.5 w-3.5" />
                </button>
              {:else}
                <select
                  class="preset-select flex-1 py-1 pl-2 pr-1 mr-1 text-xs bg-transparent border-none outline-none min-w-0 cursor-pointer"
                  bind:value={selectedPresetName}
                  on:change={() => handlePresetSelect(selectedPresetName)}
                >
                  <option value="">None</option>
                  {#each filteredPresets as preset}
                    <option value={preset.name}>{preset.name}</option>
                  {/each}
                </select>
                <button
                  on:click={startAddingPreset}
                  class="w-7 flex items-center justify-center hover:bg-background/50 border-l border-border"
                  title="Save current settings as new preset"
                >
                  <Plus class="h-3.5 w-3.5" />
                </button>
                {#if presetDirty && selectedPresetName}
                  <button
                    on:click={saveCurrentPreset}
                    class="w-7 flex items-center justify-center bg-primary text-primary-foreground hover:bg-primary/90 border-l border-border"
                    title="Save changes to '{selectedPresetName}'"
                  >
                    <Save class="h-3.5 w-3.5" />
                  </button>
                {/if}
              {/if}
            </div>
          </div>

          {#if $currentInputSource === 'tcode' || $currentInputSource === 'none'}
            <!-- Engine Selector (T-Code Only) -->
            <div class="flex h-7 pr-1 rounded border border-border overflow-hidden bg-background/50">
              <Tooltip content="Processing algorithm: v1 (queue-based ramping), v2-Smooth (averaging), v2-Balanced (interpolation, recommended), v2-Detailed (peak-preserving). Configure in General settings.">
                <span class="text-xs text-muted-foreground px-2 flex items-center gap-1 border-r border-border bg-muted/30 cursor-help">
                  Engine
                  <Info class="h-3 w-3" />
                </span>
              </Tooltip>
              <select
                class="preset-select py-1 pl-2 pr-2 text-xs bg-transparent border-none outline-none cursor-pointer"
                bind:value={$generalSettings.processingEngine}
              >
                {#each PROCESSING_ENGINES as engine}
                  <option value={engine.value}>{engine.label}</option>
                {/each}
              </select>
            </div>
          {/if}
      </div>

      <!-- Channel Controls -->
      <div class="grid grid-cols-2 gap-4">
        <ChannelControl channel="A" compact={true} shortcuts={{
          freqUp: shortcuts.channelAFreqUp,
          freqDown: shortcuts.channelAFreqDown,
          intUp: shortcuts.channelAIntUp,
          intDown: shortcuts.channelAIntDown,
          freqBalUp: shortcuts.channelAFreqBalUp,
          freqBalDown: shortcuts.channelAFreqBalDown,
          intBalUp: shortcuts.channelAIntBalUp,
          intBalDown: shortcuts.channelAIntBalDown
        }} />
        <ChannelControl channel="B" compact={true} shortcuts={{
          freqUp: shortcuts.channelBFreqUp,
          freqDown: shortcuts.channelBFreqDown,
          intUp: shortcuts.channelBIntUp,
          intDown: shortcuts.channelBIntDown,
          freqBalUp: shortcuts.channelBFreqBalUp,
          freqBalDown: shortcuts.channelBFreqBalDown,
          intBalUp: shortcuts.channelBIntBalUp,
          intBalDown: shortcuts.channelBIntBalDown
        }} />
      </div>

      <!-- Input Monitor -->
      {#if $generalSettings.showTCodeMonitor}
        <InputMonitor compact={true} />
      {/if}
    </div>
    </div>
  </div>

  <!-- Settings Modal -->
  <Dialog bind:open={settingsOpen} title="Settings">
    <TabsClassic bind:value={settingsTab} tabs={[
      { value: 'general', label: 'General' },
      { value: 'connection', label: 'Connection' },
      { value: 'buttplug', label: 'Buttplug' },
      { value: 'shortcuts', label: 'Keyboard Shortcuts' }
    ]}>
      {#if settingsTab === 'general'}
        <GeneralTab />
      {:else if settingsTab === 'buttplug'}
        <ButtplugTab />
      {:else if settingsTab === 'connection'}
        <div class="space-y-4">
          <ConnectionPanel
            compact={true}
            bind:autoOpen
            showTCodeMonitor={$generalSettings.showTCodeMonitor}
            isConnected={inputConnected}
            onConnectionChange={() => {/* Backend events handle state */}}
          />
          <BluetoothPanel
            bind:this={bluetoothPanel}
            compact={true}
            bind:selectedInterface
            bind:autoScan
            bind:autoConnect
            savedDevices={bluetoothDevicesForComponents}
            savedSelectedDevice={savedSelectedDevice}
            isConnected={outputConnected}
            onConnectionChange={() => {/* Backend events handle state */}}
          />
        </div>
      {:else if settingsTab === 'shortcuts'}
        <div class="space-y-3">
          <div class="grid grid-cols-2 gap-3">
            <div class="space-y-2">
              <h4 class="text-sm font-medium text-primary">Channel A</h4>
              <div class="space-y-1">
                <label class="flex items-center justify-between text-xs">
                  <span>Frequency Up</span>
                  <input
                    type="text"
                    bind:value={shortcuts.channelAFreqUp}
                    class="w-12 px-2 py-1 text-center bg-muted rounded"
                    maxlength="1"
                  />
                </label>
                <label class="flex items-center justify-between text-xs">
                  <span>Frequency Down</span>
                  <input
                    type="text"
                    bind:value={shortcuts.channelAFreqDown}
                    class="w-12 px-2 py-1 text-center bg-muted rounded"
                    maxlength="1"
                  />
                </label>
                <label class="flex items-center justify-between text-xs">
                  <span>Freq Balance Up</span>
                  <input
                    type="text"
                    bind:value={shortcuts.channelAFreqBalUp}
                    class="w-12 px-2 py-1 text-center bg-muted rounded"
                    maxlength="1"
                  />
                </label>
                <label class="flex items-center justify-between text-xs">
                  <span>Freq Balance Down</span>
                  <input
                    type="text"
                    bind:value={shortcuts.channelAFreqBalDown}
                    class="w-12 px-2 py-1 text-center bg-muted rounded"
                    maxlength="1"
                  />
                </label>
                <label class="flex items-center justify-between text-xs">
                  <span>Int Balance Up</span>
                  <input
                    type="text"
                    bind:value={shortcuts.channelAIntBalUp}
                    class="w-12 px-2 py-1 text-center bg-muted rounded"
                    maxlength="1"
                  />
                </label>
                <label class="flex items-center justify-between text-xs">
                  <span>Int Balance Down</span>
                  <input
                    type="text"
                    bind:value={shortcuts.channelAIntBalDown}
                    class="w-12 px-2 py-1 text-center bg-muted rounded"
                    maxlength="1"
                  />
                </label>
                <label class="flex items-center justify-between text-xs">
                  <span>Intensity Up</span>
                  <input
                    type="text"
                    bind:value={shortcuts.channelAIntUp}
                    class="w-12 px-2 py-1 text-center bg-muted rounded"
                    maxlength="1"
                  />
                </label>
                <label class="flex items-center justify-between text-xs">
                  <span>Intensity Down</span>
                  <input
                    type="text"
                    bind:value={shortcuts.channelAIntDown}
                    class="w-12 px-2 py-1 text-center bg-muted rounded"
                    maxlength="1"
                  />
                </label>
              </div>
            </div>

            <div class="space-y-2">
              <h4 class="text-sm font-medium text-secondary">Channel B</h4>
              <div class="space-y-1">
                <label class="flex items-center justify-between text-xs">
                  <span>Frequency Up</span>
                  <input
                    type="text"
                    bind:value={shortcuts.channelBFreqUp}
                    class="w-12 px-2 py-1 text-center bg-muted rounded"
                    maxlength="1"
                  />
                </label>
                <label class="flex items-center justify-between text-xs">
                  <span>Frequency Down</span>
                  <input
                    type="text"
                    bind:value={shortcuts.channelBFreqDown}
                    class="w-12 px-2 py-1 text-center bg-muted rounded"
                    maxlength="1"
                  />
                </label>
                <label class="flex items-center justify-between text-xs">
                  <span>Freq Balance Up</span>
                  <input
                    type="text"
                    bind:value={shortcuts.channelBFreqBalUp}
                    class="w-12 px-2 py-1 text-center bg-muted rounded"
                    maxlength="1"
                  />
                </label>
                <label class="flex items-center justify-between text-xs">
                  <span>Freq Balance Down</span>
                  <input
                    type="text"
                    bind:value={shortcuts.channelBFreqBalDown}
                    class="w-12 px-2 py-1 text-center bg-muted rounded"
                    maxlength="1"
                  />
                </label>
                <label class="flex items-center justify-between text-xs">
                  <span>Int Balance Up</span>
                  <input
                    type="text"
                    bind:value={shortcuts.channelBIntBalUp}
                    class="w-12 px-2 py-1 text-center bg-muted rounded"
                    maxlength="1"
                  />
                </label>
                <label class="flex items-center justify-between text-xs">
                  <span>Int Balance Down</span>
                  <input
                    type="text"
                    bind:value={shortcuts.channelBIntBalDown}
                    class="w-12 px-2 py-1 text-center bg-muted rounded"
                    maxlength="1"
                  />
                </label>
                <label class="flex items-center justify-between text-xs">
                  <span>Intensity Up</span>
                  <input
                    type="text"
                    bind:value={shortcuts.channelBIntUp}
                    class="w-12 px-2 py-1 text-center bg-muted rounded"
                    maxlength="1"
                  />
                </label>
                <label class="flex items-center justify-between text-xs">
                  <span>Intensity Down</span>
                  <input
                    type="text"
                    bind:value={shortcuts.channelBIntDown}
                    class="w-12 px-2 py-1 text-center bg-muted rounded"
                    maxlength="1"
                  />
                </label>
              </div>
            </div>
          </div>

          <div class="pt-2 border-t space-y-1">
            <h4 class="text-sm font-medium text-muted-foreground mb-2">Global</h4>
            <label class="flex items-center justify-between text-xs">
              <span>Toggle Output Pause</span>
              <input
                type="text"
                bind:value={shortcuts.toggleOutputPause}
                class="w-12 px-2 py-1 text-center bg-muted rounded"
                maxlength="5"
                placeholder="Space"
              />
            </label>
            <label class="flex items-center justify-between text-xs">
              <span>Help</span>
              <input
                type="text"
                bind:value={shortcuts.help}
                class="w-12 px-2 py-1 text-center bg-muted rounded"
                maxlength="1"
              />
            </label>
            <label class="flex items-center justify-between text-xs">
              <span>Settings</span>
              <input
                type="text"
                bind:value={shortcuts.settings}
                class="w-12 px-2 py-1 text-center bg-muted rounded"
                maxlength="1"
              />
            </label>
          </div>
        </div>
      {/if}
    </TabsClassic>

    <div class="flex justify-end gap-2 pt-3 border-t flex-shrink-0">
      <Button variant="outline" size="sm" on:click={() => settingsOpen = false}>
        Cancel
      </Button>
      <Button
        size="sm"
        class="save-button"
        on:click={() => {
          saveSettings();
          settingsOpen = false;
        }}
      >
        Save Settings
      </Button>
    </div>
  </Dialog>

  <!-- Help Modal -->
  <Dialog bind:open={helpOpen} title="Parameter Guide">
    <div class="space-y-4 text-sm overflow-y-auto scrollbar-thin flex-1 min-h-0">
      <div>
        <h4 class="font-medium text-primary mb-1">Frequency (1-200 Hz)</h4>
        <p class="text-muted-foreground">Controls the pulse frequency sent by the Coyote device. Higher frequencies create different sensations.</p>
      </div>

      <div>
        <h4 class="font-medium text-primary mb-1">Frequency Balance (0-255)</h4>
        <p class="text-muted-foreground">Controls waveform pulse width. Higher values increase low-frequency stimulation intensity.</p>
      </div>

      <div>
        <h4 class="font-medium text-primary mb-1">Intensity Balance (0-255)</h4>
        <p class="text-muted-foreground">Adjusts the feeling of high and low frequencies. Higher values strengthen low-frequency impact.</p>
      </div>

      <div>
        <h4 class="font-medium text-primary mb-1">Intensity Limits</h4>
        <p class="text-muted-foreground">Sets minimum and maximum output levels. The range slider scales input proportionally between these limits.</p>
      </div>

      <div class="pt-4 border-t">
        <h4 class="font-medium text-secondary mb-1">Processing Engine</h4>
        <ul class="space-y-1 text-muted-foreground">
          {#each PROCESSING_ENGINES as engine}
            <li>• <strong>{engine.label}:</strong> {engine.description}</li>
          {/each}
        </ul>
      </div>
    </div>
  </Dialog>

  <!-- Logs Panel -->
  <LogsPanel />
</main>

<style>
  kbd {
    font-family: 'Courier New', monospace;
    font-weight: 600;
  }

  .preset-select {
    color: hsl(var(--foreground));
  }

  .preset-select option {
    background-color: hsl(var(--background));
    color: hsl(var(--foreground));
  }

  .scrollbar-thin {
    scrollbar-width: thin;
    scrollbar-color: hsl(var(--muted-foreground) / 0.3) transparent;
  }

  .scrollbar-thin::-webkit-scrollbar {
    width: 6px;
  }

  .scrollbar-thin::-webkit-scrollbar-track {
    background: transparent;
  }

  .scrollbar-thin::-webkit-scrollbar-thumb {
    background-color: hsl(var(--muted-foreground) / 0.3);
    border-radius: 3px;
  }

  .scrollbar-thin::-webkit-scrollbar-thumb:hover {
    background-color: hsl(var(--muted-foreground) / 0.5);
  }
</style>