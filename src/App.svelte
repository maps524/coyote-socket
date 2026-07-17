<script lang="ts">
  import { onMount, onDestroy, tick } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { listen, type UnlistenFn } from '@tauri-apps/api/event';
  import { getCurrentWindow, LogicalSize } from '@tauri-apps/api/window';
  import { Settings, HelpCircle, Zap, Info, Pause, Play } from 'lucide-svelte';
  import Tooltip from './lib/components/ui/Tooltip.svelte';
  import Popover from './lib/components/ui/Popover.svelte';
  import ConnectionPanel from './lib/components/ConnectionPanel.svelte';
  import BluetoothPanel, { type BluetoothPanelState, type BluetoothDevice } from './lib/components/BluetoothPanel.svelte';
  import InputStatusPill from './lib/components/InputStatusPill.svelte';
  import GamepadStatusPill from './lib/components/GamepadStatusPill.svelte';
  import { startGamepadStatusSync, stopGamepadStatusSync } from '$lib/stores/gamepadStatus';
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
  import GamepadBindControl from './lib/components/ui/GamepadBindControl.svelte';
  import { outputOptions, connectionStatus, PROCESSING_ENGINES, PEAK_FILL_STRATEGIES, type ProcessingEngine, type PeakFillStrategy } from './lib/stores/connection.js';
  import { channelA, channelB } from './lib/stores/channels.js';
  import { generalSettings } from './lib/stores/generalSettings.js';
  import { startResolvedTracking, stopResolvedTracking } from './lib/stores/resolvedState.js';
  import { startInputBusTracking, stopInputBusTracking, clearInputBus } from './lib/stores/inputBus.js';
  import {
    startStateSync,
    stopStateSync,
    getFullState,
    refreshConnectionStatus,
    connectionState,
    type FullAppState
  } from './lib/stores/stateSync.js';
  import { currentInputSource } from './lib/stores/inputSource.js';
  import { presetSelectionStore, setSelectedPreset, clearSelectedPreset } from './lib/stores/presetSelection.js';
  import type { AppSettings, ChannelPreset, ChannelSettings, ChordPart, GamepadBinding, GamepadBindings, PresetEcosystem } from './lib/types/settings';
  import type { NoInputBehavior } from './lib/types/modulation';
  import { coyoteService } from './lib/services/CoyoteService.js';
  import { Plus, Save, X, ListOrdered, GripVertical, Trash2 } from 'lucide-svelte';
  import { dndzone, type DndEvent } from 'svelte-dnd-action';
  import { flip } from 'svelte/animate';

  let settingsOpen = $state(false);
  let helpOpen = $state(false);
  let outputPaused = $state(false);

  // Connection state is now derived from the connectionState store (backend is source of truth)
  // These reactive declarations automatically update when the store changes
  let inputConnected = $derived($connectionState.websocket_running);
  let outputConnected = $derived($connectionState.bluetooth_connected);
  let batteryLevel = $derived($connectionState.battery_level);

  let settingsTab = $state('general');

  // Debounce rates (ms)
  const UPDATE_DEBOUNCE = 50;   // Backend state updates (real-time responsiveness)
  const SAVE_DEBOUNCE = 500;    // File persistence (reduce disk IO)

  // Update timers for backend state (50ms debounce) — per-channel timers live in
  // channelATimers/channelBTimers below, alongside scheduleChannelSync.
  let outputUpdateTimer: ReturnType<typeof setTimeout> | null = null;

  // Save timers for file persistence (500ms debounce)
  let connectionSaveTimer: ReturnType<typeof setTimeout> | null = null;
  let bluetoothSaveTimer: ReturnType<typeof setTimeout> | null = null;
  let outputSaveTimer: ReturnType<typeof setTimeout> | null = null;
  let shortcutsSaveTimer: ReturnType<typeof setTimeout> | null = null;

  // Keyboard shortcuts configuration
  let shortcuts = $state({
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
    toggleOutputPause: ' ',  // Space bar
    cyclePreset: '',
    cyclePresetForward: '',
    cyclePresetBack: ''
  });

  // Gamepad bindings (orthogonal to keyboard shortcuts). Each action may have
  // at most one gamepad binding. Loaded from backend on mount, saved on change.
  let gamepadBindings: GamepadBindings = $state({});
  let gamepadBindingsSaveTimer: ReturnType<typeof setTimeout> | null = null;
  let gamepadEngine: 'off' | 'gilrs' | 'xinput' = $state('xinput');
  let gamepadStickSensitivity = $state(1.0);
  let sensitivitySaveTimer: ReturnType<typeof setTimeout> | null = null;
  let gamepadRepeatDelay = $state(400);
  let gamepadRepeatInterval = $state(100);
  let repeatSaveTimer: ReturnType<typeof setTimeout> | null = null;

  // When non-null, the rebind UI is capturing the next keyboard/gamepad event
  // for this action — keyboard handler short-circuits to avoid firing the action.
  type GamepadRawEvent =
    | { kind: 'button'; index: number; released?: boolean }
    | { kind: 'axis'; index: number; dir: 'pos' | 'neg'; threshold: number; released?: boolean };

  let rebindCapture:
    | { action: string; source: 'gamepad'; parts: ChordPart[]; heldIds: Set<string> }
    | null = $state(null);

  function partId(p: { kind: string; index: number; dir?: string }): string {
    return `${p.kind}-${p.index}-${p.dir ?? ''}`;
  }

  function stripReleasedFromEvent(ev: GamepadRawEvent): ChordPart {
    if (ev.kind === 'button') return { kind: 'button', index: ev.index };
    return { kind: 'axis', index: ev.index, dir: ev.dir, threshold: ev.threshold };
  }
  let unlistenInputAction: UnlistenFn | null = null;
  let unlistenGamepadRaw: UnlistenFn | null = null;

  // Action display order + label for the rebind UI.
  // `action` is the camelCase name; matches both keyboard shortcuts (when
  // present) and gamepad binding map keys.
  const ACTION_ROWS: { action: string; label: string; group: 'A' | 'B' | 'global' }[] = [
    { action: 'channelAFreqUp',         label: 'Frequency Up',     group: 'A' },
    { action: 'channelAFreqDown',       label: 'Frequency Down',   group: 'A' },
    { action: 'channelAFreqRangeMinUp',   label: 'Freq Range Min Up',   group: 'A' },
    { action: 'channelAFreqRangeMinDown', label: 'Freq Range Min Down', group: 'A' },
    { action: 'channelAFreqRangeMaxUp',   label: 'Freq Range Max Up',   group: 'A' },
    { action: 'channelAFreqRangeMaxDown', label: 'Freq Range Max Down', group: 'A' },
    { action: 'channelAIntUp',          label: 'Intensity Up',     group: 'A' },
    { action: 'channelAIntDown',        label: 'Intensity Down',   group: 'A' },
    { action: 'channelAIntRangeMinUp',   label: 'Int Range Min Up',  group: 'A' },
    { action: 'channelAIntRangeMinDown', label: 'Int Range Min Down', group: 'A' },
    { action: 'channelAIntRangeMaxUp',   label: 'Int Range Max Up',  group: 'A' },
    { action: 'channelAIntRangeMaxDown', label: 'Int Range Max Down', group: 'A' },
    { action: 'channelAFreqBalUp',      label: 'Freq Balance Up',  group: 'A' },
    { action: 'channelAFreqBalDown',    label: 'Freq Balance Down', group: 'A' },
    { action: 'channelAIntBalUp',       label: 'Int Balance Up',   group: 'A' },
    { action: 'channelAIntBalDown',     label: 'Int Balance Down', group: 'A' },
    { action: 'channelBFreqUp',         label: 'Frequency Up',     group: 'B' },
    { action: 'channelBFreqDown',       label: 'Frequency Down',   group: 'B' },
    { action: 'channelBFreqRangeMinUp',   label: 'Freq Range Min Up',   group: 'B' },
    { action: 'channelBFreqRangeMinDown', label: 'Freq Range Min Down', group: 'B' },
    { action: 'channelBFreqRangeMaxUp',   label: 'Freq Range Max Up',   group: 'B' },
    { action: 'channelBFreqRangeMaxDown', label: 'Freq Range Max Down', group: 'B' },
    { action: 'channelBIntUp',          label: 'Intensity Up',     group: 'B' },
    { action: 'channelBIntDown',        label: 'Intensity Down',   group: 'B' },
    { action: 'channelBIntRangeMinUp',   label: 'Int Range Min Up',  group: 'B' },
    { action: 'channelBIntRangeMinDown', label: 'Int Range Min Down', group: 'B' },
    { action: 'channelBIntRangeMaxUp',   label: 'Int Range Max Up',  group: 'B' },
    { action: 'channelBIntRangeMaxDown', label: 'Int Range Max Down', group: 'B' },
    { action: 'channelBFreqBalUp',      label: 'Freq Balance Up',  group: 'B' },
    { action: 'channelBFreqBalDown',    label: 'Freq Balance Down', group: 'B' },
    { action: 'channelBIntBalUp',       label: 'Int Balance Up',   group: 'B' },
    { action: 'channelBIntBalDown',     label: 'Int Balance Down', group: 'B' },
    { action: 'toggleOutputPause',      label: 'Toggle Output Pause', group: 'global' },
    { action: 'cyclePreset',            label: 'Cycle Preset',     group: 'global' },
    { action: 'cyclePresetForward',     label: 'Cycle Preset Forward', group: 'global' },
    { action: 'cyclePresetBack',        label: 'Cycle Preset Back', group: 'global' },
    { action: 'help',                   label: 'Help',             group: 'global' },
    { action: 'settings',               label: 'Settings',         group: 'global' },
  ];

  function startRebindGamepad(action: string) {
    rebindCapture = { action, source: 'gamepad', parts: [], heldIds: new Set() };
  }

  function saveCombo() {
    if (!rebindCapture || rebindCapture.parts.length === 0) {
      rebindCapture = null;
      return;
    }
    const action = rebindCapture.action;
    const binding: GamepadBinding = rebindCapture.parts.length === 1
      ? rebindCapture.parts[0] as GamepadBinding
      : { kind: 'combo', parts: rebindCapture.parts };
    gamepadBindings = { ...gamepadBindings, [action]: binding };
    rebindCapture = null;
  }

  function clearGamepadBinding(action: string) {
    const next = { ...gamepadBindings };
    delete next[action];
    gamepadBindings = next;
  }

  function cancelRebind() {
    rebindCapture = null;
  }

  function shortcutKeyFor(action: string): string | undefined {
    return (shortcuts as Record<string, string>)[action];
  }

  async function changeGamepadEngine(engine: 'off' | 'gilrs' | 'xinput') {
    gamepadEngine = engine;
    try {
      await invoke('set_gamepad_engine', { engine });
    } catch (e) {
      console.error('[Gamepad] Failed to switch engine:', e);
    }
  }

  function onSensitivityInput(e: Event) {
    const target = e.currentTarget as HTMLInputElement;
    changeStickSensitivity(parseFloat(target.value));
  }

  function changeStickSensitivity(value: number) {
    gamepadStickSensitivity = value;
    if (sensitivitySaveTimer) clearTimeout(sensitivitySaveTimer);
    sensitivitySaveTimer = setTimeout(() => {
      invoke('set_gamepad_stick_sensitivity', { value })
        .catch((e) => console.error('[Gamepad] Failed to save sensitivity:', e));
    }, 250);
  }

  function scheduleRepeatSave() {
    if (repeatSaveTimer) clearTimeout(repeatSaveTimer);
    repeatSaveTimer = setTimeout(() => {
      invoke('set_gamepad_button_repeat', {
        delayMs: Math.round(gamepadRepeatDelay),
        intervalMs: Math.round(gamepadRepeatInterval),
      }).catch((e) => console.error('[Gamepad] Failed to save repeat:', e));
    }, 250);
  }

  function onRepeatDelayInput(e: Event) {
    gamepadRepeatDelay = parseFloat((e.currentTarget as HTMLInputElement).value);
    scheduleRepeatSave();
  }

  function onRepeatIntervalInput(e: Event) {
    gamepadRepeatInterval = parseFloat((e.currentTarget as HTMLInputElement).value);
    scheduleRepeatSave();
  }

  function onGamepadEngineChange(e: Event) {
    const target = e.currentTarget as HTMLSelectElement;
    changeGamepadEngine(target.value as 'off' | 'gilrs' | 'xinput');
  }

  // Connection settings
  let websocketPort = $state(12346);
  let selectedInterface = $state(0);
  let autoScan = $state(true);
  let autoOpen = $state(true);
  let autoConnect = $state(true);  // Auto-connect to Coyote when found
  let settingsLoaded = $state(false);  // Flag to prevent auto-save before settings are loaded
  let hmrReloading = $state(true);  // Start true - prevents reactive sync until after initial load completes

  // savedSelectedDevice is a user preference (last device they connected to)
  let savedSelectedDevice = $state('');
  let inputPill: InputStatusPill = $state()!;
  let outputPill: OutputStatusPill = $state()!;
  let bluetoothPanel: BluetoothPanel = $state()!;

  // Preset management
  let presets: ChannelPreset[] = $state([]);
  let isAddingPreset = $state(false);
  let newPresetName = $state('');
  let presetDirty = $state(false);
  let lastSavedPresetState: { channelA: ChannelSettings, channelB: ChannelSettings } | null = $state(null);

  // Event listener unsubscribe functions
  let unlistenOutputPause: UnlistenFn | null = null;

  // Engine info popover — shows the description of the currently-selected
  // engine on click. Description text comes from PROCESSING_ENGINES.
  let engineInfoOpen = $state(false);
  let activeEngine = $derived(PROCESSING_ENGINES.find(e => e.value === $generalSettings.processingEngine));

  // Derive current ecosystem from input source. Lovense input is funneled
  // through the Buttplug feature pipeline, so it shares the buttplug ecosystem
  // for preset filtering and channel-routing UI.
  let currentEcosystem = $derived($currentInputSource === 'tcode'
    ? ('tcode' as PresetEcosystem)
    : ($currentInputSource === 'buttplug' || $currentInputSource === 'lovense')
      ? ('buttplug' as PresetEcosystem)
      : ('tcode' as PresetEcosystem));  // Default to tcode when no input

  // Get selected preset name from store based on current ecosystem
  let selectedPresetName = $derived($presetSelectionStore[currentEcosystem]);

  // Filter presets based on current ecosystem
  let filteredPresets = $derived(presets.filter(p => p.ecosystem === currentEcosystem));

  // Discovered Bluetooth devices come from the backend (not persisted to settings)
  let bluetoothDevicesForComponents = $derived($connectionState.discovered_devices.map(d => ({
    address: d.address,
    name: d.name ?? undefined,
    product: d.product ?? undefined,
    rssi: d.rssi ?? undefined
  })));

  onMount(async () => {
    console.log('CoyoteSocket application starting...');

    // FIRST: Start state sync and get live connection state immediately
    // This ensures connection status is shown without delay after HMR
    await startStateSync();
    // Gamepad connect/disconnect feed for the nav status pill
    await startGamepadStatusSync();
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
      autoOpen = settings.connection.autoOpen ?? true;

      // Apply general settings (including showTCodeMonitor from connection settings for backwards compatibility)
      $generalSettings = {
        noInputBehavior: (settings.general?.noInputBehavior ?? 'hold') as NoInputBehavior,
        noInputDecayMs: settings.general?.noInputDecayMs ?? 1000,
        updateRateMs: settings.general?.updateRateMs ?? 50,
        saveRateMs: settings.general?.saveRateMs ?? 500,
        showTCodeMonitor: settings.connection.showTcodeMonitor ?? false,
        processingEngine: (settings.output.processingEngine as ProcessingEngine) ?? 'v2-balanced',
        peakFill: (settings.output.peakFill as PeakFillStrategy) ?? 'forward',
        channelAMaxIntensity: settings.general?.channelAMaxIntensity ?? 200,
        channelBMaxIntensity: settings.general?.channelBMaxIntensity ?? 200
      };

      // Apply bluetooth settings (discovered devices come from backend, not settings)
      selectedInterface = settings.bluetooth.selectedInterface ?? 0;
      autoScan = settings.bluetooth.autoScan ?? true;
      autoConnect = settings.bluetooth.autoConnect ?? true;
      savedSelectedDevice = settings.bluetooth.lastDevice || '';

      // Apply output settings
      $outputOptions = {
        processingEngine: (settings.output.processingEngine as ProcessingEngine) ?? 'v2-balanced',
        peakFill: (settings.output.peakFill as PeakFillStrategy) ?? 'forward'
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
        frequencySource: { ...freqA, curve: freqA.curve as any },
        frequencyBalanceSource: { ...freqBalA, curve: freqBalA.curve as any },
        intensityBalanceSource: { ...intBalA, curve: intBalA.curve as any },
        intensitySource: { ...intA, curve: intA.curve as any }
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
        frequencySource: { ...freqB, curve: freqB.curve as any },
        frequencyBalanceSource: { ...freqBalB, curve: freqBalB.curve as any },
        intensityBalanceSource: { ...intBalB, curve: intBalB.curve as any },
        intensitySource: { ...intB, curve: intB.curve as any }
      };

      // Apply keyboard shortcuts
      shortcuts = { ...settings.shortcuts };

      // Apply gamepad bindings (may be missing on legacy installs)
      gamepadBindings = { ...(settings.gamepadBindings ?? {}) };

      // Apply gamepad engine selection
      gamepadEngine = (settings.general?.gamepadEngine ?? 'xinput') as typeof gamepadEngine;
      gamepadStickSensitivity = settings.general?.gamepadStickSensitivity ?? 1.0;
      gamepadRepeatDelay = settings.general?.gamepadButtonRepeatDelayMs ?? 400;
      gamepadRepeatInterval = settings.general?.gamepadButtonRepeatIntervalMs ?? 100;

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

      // Listen for gamepad-triggered actions from backend.
      // For continuous emissions (stick/trigger), apply user's sensitivity multiplier.
      unlistenInputAction = await listen<{ action: string; source: string; magnitude?: number; continuous?: boolean }>('input-action', (event) => {
        if (rebindCapture && rebindCapture.source === 'gamepad') return;
        let m = event.payload.magnitude ?? 1;
        if (event.payload.continuous) {
          m *= gamepadStickSensitivity;
        }
        dispatchAction(event.payload.action, m);
      });

      // Raw gamepad events used only by the rebind UI.
      // Capture rules:
      //   - First press starts the combo: parts = [press].
      //   - Additional presses while any part is still held: append (deduped).
      //   - Release events maintain the held set; when it empties, the next
      //     press resets parts (lets users redo without hitting Cancel).
      unlistenGamepadRaw = await listen<GamepadRawEvent>('gamepad-raw', (event) => {
        if (!rebindCapture || rebindCapture.source !== 'gamepad') return;
        const ev = event.payload;
        const id = partId(ev);

        if (ev.released) {
          const next = new Set(rebindCapture.heldIds);
          next.delete(id);
          rebindCapture = { ...rebindCapture, heldIds: next };
          return;
        }

        let parts = rebindCapture.parts;
        if (rebindCapture.heldIds.size === 0) {
          parts = [stripReleasedFromEvent(ev)];
        } else if (!parts.some(p => partId(p) === id)) {
          parts = [...parts, stripReleasedFromEvent(ev)];
        }
        const heldIds = new Set(rebindCapture.heldIds);
        heldIds.add(id);
        rebindCapture = { ...rebindCapture, parts, heldIds };
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

    // Start resolved-state tracking for range slider indicators (10Hz
    // post-resolver snapshot from backend, RAF-smoothed for display).
    startResolvedTracking();
    // Start input bus tracking (per-write `bus-update` events for the
    // input monitor + transforms editor's axis discovery).
    startInputBusTracking();

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
      // Delay to let Tauri + Windows BLE stack settle. 1s wasn't enough
      // on a cold launch — the first scan would return empty even though
      // the device was advertising, forcing a manual refresh.
      setTimeout(async () => {
        await scanAndConnect();
      }, 2000);
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

  // Scan for Bluetooth devices and optionally auto-connect.
  // Windows BLE stack often returns an empty scan on the first attempt
  // right after app launch (radio cold-start, OS hasn't enumerated nearby
  // peripherals yet). Retry up to 3 times before giving up — this matches
  // what the user otherwise has to do by hand with the refresh button.
  async function scanAndConnect() {
    const adapterIndex = Number(selectedInterface) || 0;
    const MAX_ATTEMPTS = 3;

    for (let attempt = 1; attempt <= MAX_ATTEMPTS; attempt++) {
      try {
        console.log(`[auto-scan] attempt ${attempt}/${MAX_ATTEMPTS} on adapter ${adapterIndex}`);
        const devices = await invoke<BluetoothDevice[]>('scan_bluetooth_devices', {
          adapterIndex
        });
        console.log(`[auto-scan] attempt ${attempt} found ${devices.length} device(s):`, devices);

        // Push the (possibly empty) discovered list to the store so the
        // panel UI reflects backend state on every attempt.
        await refreshConnectionStatus();

        const coyoteDevice = devices.find(d =>
          d.name?.includes('COYOTE') ||
          d.name?.includes('DG-LAB') ||
          d.name?.includes('47L')
        );

        if (coyoteDevice) {
          savedSelectedDevice = coyoteDevice.address;
          console.log('[auto-scan] found Coyote device:', coyoteDevice);

          if (autoConnect && !outputConnected) {
            console.log('[auto-scan] auto-connecting...');
            try {
              const result = await invoke<string>('connect_bluetooth_device', {
                adapterIndex,
                address: coyoteDevice.address
              });
              console.log('[auto-scan] connect result:', result);
            } catch (connectError) {
              console.error('[auto-scan] auto-connect failed:', connectError);
            }
          }
          return;
        }

        if (attempt < MAX_ATTEMPTS) {
          console.log('[auto-scan] no Coyote yet — retrying after 1.5s');
          await new Promise(r => setTimeout(r, 1500));
        } else {
          console.log('[auto-scan] gave up after', MAX_ATTEMPTS, 'attempts');
        }
      } catch (error) {
        console.error(`[auto-scan] attempt ${attempt} failed:`, error);
        if (attempt < MAX_ATTEMPTS) {
          await new Promise(r => setTimeout(r, 1500));
        }
      }
    }
  }

  onDestroy(() => {
    // Set HMR flag first to prevent reactive statements from syncing during reload
    hmrReloading = true;

    coyoteService.destroy();
    // Stop resolved-state tracking
    stopResolvedTracking();
    // Stop input-bus tracking + drop the in-memory axis map so a fresh
    // mount doesn't carry stale samples across an HMR reload.
    stopInputBusTracking();
    clearInputBus();
    // Stop state sync event listeners
    stopStateSync();
    stopGamepadStatusSync();
    // Clear output pause event listener
    if (unlistenOutputPause) unlistenOutputPause();
    // Clear all update timers (50ms)
    if (outputUpdateTimer) clearTimeout(outputUpdateTimer);
    if (channelATimers.update.current) clearTimeout(channelATimers.update.current);
    if (channelBTimers.update.current) clearTimeout(channelBTimers.update.current);
    // Clear all save timers (500ms)
    if (connectionSaveTimer) clearTimeout(connectionSaveTimer);
    if (generalSaveTimer) clearTimeout(generalSaveTimer);
    if (bluetoothSaveTimer) clearTimeout(bluetoothSaveTimer);
    if (outputSaveTimer) clearTimeout(outputSaveTimer);
    if (channelATimers.save.current) clearTimeout(channelATimers.save.current);
    if (channelBTimers.save.current) clearTimeout(channelBTimers.save.current);
    if (shortcutsSaveTimer) clearTimeout(shortcutsSaveTimer);
    if (gamepadBindingsSaveTimer) clearTimeout(gamepadBindingsSaveTimer);
    if (sensitivitySaveTimer) clearTimeout(sensitivitySaveTimer);
    if (repeatSaveTimer) clearTimeout(repeatSaveTimer);
    if (unlistenInputAction) unlistenInputAction();
    if (unlistenGamepadRaw) unlistenGamepadRaw();
  });

  // Window height constants
  const WINDOW_HEIGHT_COMPACT = 425;
  const WINDOW_HEIGHT_WITH_MONITOR = 600;

  // Resize window when T-Code monitor is toggled
  // Plain (non-reactive) tracker: compared inside the effect below to detect
  // showTCodeMonitor transitions. Must NOT be $state, or the effect that reads
  // and writes it would recurse (legacy_recursive_reactive_block).
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
  $effect(() => {
    if (lastTCodeMonitorState !== null && lastTCodeMonitorState !== $generalSettings.showTCodeMonitor) {
      resizeWindowForMonitor($generalSettings.showTCodeMonitor);
    }
    lastTCodeMonitorState = $generalSettings.showTCodeMonitor;
  });

  // Debounced save for connection settings
  $effect(() => {
    if (settingsLoaded && !hmrReloading) {
      // showTCodeMonitor must be tracked synchronously: a $effect only depends on
      // signals read during its sync run, not ones read later inside the timeout
      // callback. Without it here, toggling the monitor never re-saved the
      // connection block (the field it loads from), so the pref didn't persist.
      const _trackConnectionChanges = [websocketPort, autoOpen, $generalSettings.showTCodeMonitor];
      if (connectionSaveTimer) clearTimeout(connectionSaveTimer);
      connectionSaveTimer = setTimeout(() => {
        invoke('save_connection_settings', {
          websocketPort,
          autoOpen,
          showTcodeMonitor: $generalSettings.showTCodeMonitor
        }).catch((e) => console.error('[Settings] Failed to save connection settings:', e));
      }, 500);
    }
  });

  // Debounced save for general settings
  let generalSaveTimer: ReturnType<typeof setTimeout> | null = null;
  $effect(() => {
    if (settingsLoaded && !hmrReloading && $generalSettings) {
      if (generalSaveTimer) clearTimeout(generalSaveTimer);
      generalSaveTimer = setTimeout(() => {
        // Note: peakFill persists via save_output_settings (OutputSettings), not here.
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
  });

  // Debounced save for bluetooth settings (user preferences only, not discovered devices)
  $effect(() => {
    if (settingsLoaded && !hmrReloading) {
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
  });

  // Sync output options to backend (50ms) and save to file (500ms).
  $effect(() => {
    if ($generalSettings && settingsLoaded && !hmrReloading) {
      if (outputUpdateTimer) clearTimeout(outputUpdateTimer);
      outputUpdateTimer = setTimeout(() => {
        invoke('update_output_options', {
          engine: $generalSettings.processingEngine ?? 'v2-balanced',
          peakFill: $generalSettings.peakFill ?? 'forward'
        }).catch(() => {});
      }, UPDATE_DEBOUNCE);

      if (outputSaveTimer) clearTimeout(outputSaveTimer);
      outputSaveTimer = setTimeout(() => {
        invoke('save_output_settings', {
          processingEngine: $generalSettings.processingEngine ?? 'v2-balanced',
          peakFill: $generalSettings.peakFill ?? 'forward'
        }).catch((e) => console.error('[Settings] Failed to save output settings:', e));
      }, SAVE_DEBOUNCE);
    }
  });

  // Build the full ChannelSettings payload shipped to the backend (disk + runtime).
  // Same shape for A and B; default source axis differs per channel.
  function buildChannelSettingsPayload(
    ch: import('./lib/stores/channels.js').ChannelParams,
    defaultAxis: 'L0' | 'R2'
  ) {
    // Spread each source first, then backfill defaults for missing fields.
    // The spread carries every optional field (midpoint, delayMs, transforms,
    // and anything added later) so the persist payload can't silently drop a
    // field the source type gains.
    return {
      frequencySource: {
        ...ch.frequencySource,
        type: ch.frequencySource?.type ?? 'static',
        staticValue: ch.frequencySource?.staticValue ?? ch.frequency,
        sourceAxis: ch.frequencySource?.sourceAxis ?? defaultAxis,
        rangeMin: ch.frequencySource?.rangeMin ?? 1,
        rangeMax: ch.frequencySource?.rangeMax ?? 200,
        curve: ch.frequencySource?.curve ?? 'linear',
        curveStrength: ch.frequencySource?.curveStrength ?? 2.0
      },
      frequencyBalanceSource: {
        ...ch.frequencyBalanceSource,
        type: ch.frequencyBalanceSource?.type ?? 'static',
        staticValue: ch.frequencyBalanceSource?.staticValue ?? ch.frequencyBalance,
        sourceAxis: ch.frequencyBalanceSource?.sourceAxis ?? defaultAxis,
        rangeMin: ch.frequencyBalanceSource?.rangeMin ?? 0,
        rangeMax: ch.frequencyBalanceSource?.rangeMax ?? 255,
        curve: ch.frequencyBalanceSource?.curve ?? 'linear',
        curveStrength: ch.frequencyBalanceSource?.curveStrength ?? 2.0
      },
      intensityBalanceSource: {
        ...ch.intensityBalanceSource,
        type: ch.intensityBalanceSource?.type ?? 'static',
        staticValue: ch.intensityBalanceSource?.staticValue ?? ch.intensityBalance,
        sourceAxis: ch.intensityBalanceSource?.sourceAxis ?? defaultAxis,
        rangeMin: ch.intensityBalanceSource?.rangeMin ?? 0,
        rangeMax: ch.intensityBalanceSource?.rangeMax ?? 255,
        curve: ch.intensityBalanceSource?.curve ?? 'linear',
        curveStrength: ch.intensityBalanceSource?.curveStrength ?? 2.0
      },
      intensitySource: {
        ...ch.intensitySource,
        type: ch.intensitySource?.type ?? 'linked',
        staticValue: ch.intensitySource?.staticValue ?? 100,
        sourceAxis: ch.intensitySource?.sourceAxis ?? defaultAxis,
        rangeMin: ch.intensitySource?.rangeMin ?? ch.rangeMin,
        rangeMax: ch.intensitySource?.rangeMax ?? ch.rangeMax,
        curve: ch.intensitySource?.curve ?? 'linear',
        curveStrength: ch.intensitySource?.curveStrength ?? 2.0
      }
    };
  }

  // Schedules the 50ms fast runtime update and the 500ms disk persistence for one channel.
  // Both paths send the full channelSettings payload so backend processing state and
  // disk file stay consistent; the 50ms path skips disk I/O.
  function scheduleChannelSync(
    letter: 'A' | 'B',
    ch: import('./lib/stores/channels.js').ChannelParams,
    defaultAxis: 'L0' | 'R2',
    updateTimerRef: {
      current: ReturnType<typeof setTimeout> | null;
      lastFireMs: number;
      pendingSettings: ReturnType<typeof buildChannelSettingsPayload> | null;
    },
    saveTimerRef: { current: ReturnType<typeof setTimeout> | null }
  ) {
    const channelSettings = buildChannelSettingsPayload(ch, defaultAxis);

    // Runtime sync uses a LEADING+TRAILING throttle so users get live
    // feedback while dragging: the first event fires immediately, then
    // coalesces subsequent events into one trailing call at the 50ms
    // boundary. This caps runtime writes at ~20/sec regardless of drag
    // speed while keeping perceived latency near zero.
    const now = Date.now();
    const elapsed = now - updateTimerRef.lastFireMs;

    if (elapsed >= UPDATE_DEBOUNCE) {
      // Leading edge: fire right away with the current payload.
      if (updateTimerRef.current) {
        clearTimeout(updateTimerRef.current);
        updateTimerRef.current = null;
      }
      updateTimerRef.lastFireMs = now;
      updateTimerRef.pendingSettings = null;
      invoke('apply_channel_config', { channel: letter, channelSettings, persist: false }).catch(() => {});
    } else {
      // Within throttle window — stash latest payload; schedule trailing
      // fire only if one isn't already pending. Latest value always wins.
      updateTimerRef.pendingSettings = channelSettings;
      if (!updateTimerRef.current) {
        const delay = UPDATE_DEBOUNCE - elapsed;
        updateTimerRef.current = setTimeout(() => {
          updateTimerRef.current = null;
          const payload = updateTimerRef.pendingSettings;
          updateTimerRef.pendingSettings = null;
          updateTimerRef.lastFireMs = Date.now();
          if (payload) {
            invoke('apply_channel_config', { channel: letter, channelSettings: payload, persist: false }).catch(() => {});
          }
        }, delay);
      }
    }

    // Disk persistence stays a trailing debounce — disk writes don't need
    // live feedback, and reset-on-change keeps I/O to one write per
    // settling period.
    if (saveTimerRef.current) clearTimeout(saveTimerRef.current);
    saveTimerRef.current = setTimeout(() => {
      invoke('apply_channel_config', { channel: letter, channelSettings, persist: true })
        .catch((e) => console.error(`[Settings] Failed to save channel ${letter} settings:`, e));
    }, SAVE_DEBOUNCE);
  }

  // Shared refs let the helper clear/reuse the same timeout slots across reactive fires.
  // `lastFireMs` + `pendingSettings` drive the throttle; `current` holds the
  // trailing timer.
  const channelATimers = {
    update: {
      current: null as ReturnType<typeof setTimeout> | null,
      lastFireMs: 0,
      pendingSettings: null as ReturnType<typeof buildChannelSettingsPayload> | null
    },
    save: { current: null as ReturnType<typeof setTimeout> | null }
  };
  const channelBTimers = {
    update: {
      current: null as ReturnType<typeof setTimeout> | null,
      lastFireMs: 0,
      pendingSettings: null as ReturnType<typeof buildChannelSettingsPayload> | null
    },
    save: { current: null as ReturnType<typeof setTimeout> | null }
  };

  $effect(() => {
    if ($channelA && settingsLoaded && !hmrReloading) {
      scheduleChannelSync('A', $channelA, 'L0', channelATimers.update, channelATimers.save);
    }
  });
  $effect(() => {
    if ($channelB && settingsLoaded && !hmrReloading) {
      scheduleChannelSync('B', $channelB, 'R2', channelBTimers.update, channelBTimers.save);
    }
  });

  // Save shortcuts when they change
  $effect(() => {
    if (settingsLoaded && !hmrReloading && shortcuts) {
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
          toggleOutputPause: shortcuts.toggleOutputPause,
          cyclePreset: shortcuts.cyclePreset,
          cyclePresetForward: shortcuts.cyclePresetForward,
          cyclePresetBack: shortcuts.cyclePresetBack
        }).catch((e) => console.error('[Settings] Failed to save shortcuts:', e));
      }, 500);
    }
  });

  // Save gamepad bindings when they change
  $effect(() => {
    if (settingsLoaded && !hmrReloading && gamepadBindings) {
      if (gamepadBindingsSaveTimer) clearTimeout(gamepadBindingsSaveTimer);
      gamepadBindingsSaveTimer = setTimeout(() => {
        invoke('save_gamepad_bindings', { bindings: gamepadBindings })
          .catch((e) => console.error('[Settings] Failed to save gamepad bindings:', e));
      }, 500);
    }
  });

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

  // Action dispatch — single source of truth for every bindable action.
  // Both keyboard shortcuts (DOM keydown) and gamepad bindings (Tauri
  // 'input-action' event) funnel through here. `magnitude` is 0..1 from
  // continuous axis bindings (stick deflection); 1.0 for discrete events.
  function dispatchAction(action: string, magnitude: number = 1) {
    // Discrete action steps scaled by magnitude. Only applies when magnitude < 1.
    const m = magnitude;
    switch (action) {
      case 'channelAFreqUp':       adjustFrequency('A', 'up', m); break;
      case 'channelAFreqDown':     adjustFrequency('A', 'down', m); break;
      case 'channelAIntUp':        adjustIntensity('A', 5 * m); break;
      case 'channelAIntDown':      adjustIntensity('A', -5 * m); break;
      case 'channelAFreqBalUp':    adjustBalance('A', 'frequency', 5 * m); break;
      case 'channelAFreqBalDown':  adjustBalance('A', 'frequency', -5 * m); break;
      case 'channelAIntBalUp':     adjustBalance('A', 'intensity', 5 * m); break;
      case 'channelAIntBalDown':   adjustBalance('A', 'intensity', -5 * m); break;
      case 'channelBFreqUp':       adjustFrequency('B', 'up', m); break;
      case 'channelBFreqDown':     adjustFrequency('B', 'down', m); break;
      case 'channelBIntUp':        adjustIntensity('B', 5 * m); break;
      case 'channelBIntDown':      adjustIntensity('B', -5 * m); break;
      case 'channelBFreqBalUp':    adjustBalance('B', 'frequency', 5 * m); break;
      case 'channelBFreqBalDown':  adjustBalance('B', 'frequency', -5 * m); break;
      case 'channelBIntBalUp':     adjustBalance('B', 'intensity', 5 * m); break;
      case 'channelBIntBalDown':   adjustBalance('B', 'intensity', -5 * m); break;
      case 'channelAIntRangeMinUp':   adjustIntensityRangeBound('A', 'min', 5 * m); break;
      case 'channelAIntRangeMinDown': adjustIntensityRangeBound('A', 'min', -5 * m); break;
      case 'channelAIntRangeMaxUp':   adjustIntensityRangeBound('A', 'max', 5 * m); break;
      case 'channelAIntRangeMaxDown': adjustIntensityRangeBound('A', 'max', -5 * m); break;
      case 'channelBIntRangeMinUp':   adjustIntensityRangeBound('B', 'min', 5 * m); break;
      case 'channelBIntRangeMinDown': adjustIntensityRangeBound('B', 'min', -5 * m); break;
      case 'channelBIntRangeMaxUp':   adjustIntensityRangeBound('B', 'max', 5 * m); break;
      case 'channelBIntRangeMaxDown': adjustIntensityRangeBound('B', 'max', -5 * m); break;
      case 'channelAFreqRangeMinUp':   adjustFrequencyRangeBound('A', 'min', 5 * m); break;
      case 'channelAFreqRangeMinDown': adjustFrequencyRangeBound('A', 'min', -5 * m); break;
      case 'channelAFreqRangeMaxUp':   adjustFrequencyRangeBound('A', 'max', 5 * m); break;
      case 'channelAFreqRangeMaxDown': adjustFrequencyRangeBound('A', 'max', -5 * m); break;
      case 'channelBFreqRangeMinUp':   adjustFrequencyRangeBound('B', 'min', 5 * m); break;
      case 'channelBFreqRangeMinDown': adjustFrequencyRangeBound('B', 'min', -5 * m); break;
      case 'channelBFreqRangeMaxUp':   adjustFrequencyRangeBound('B', 'max', 5 * m); break;
      case 'channelBFreqRangeMaxDown': adjustFrequencyRangeBound('B', 'max', -5 * m); break;
      case 'help':                 helpOpen = true; break;
      case 'settings':             settingsOpen = true; break;
      case 'toggleOutputPause':    toggleOutputPause(); break;
      case 'cyclePreset':          cyclePresets(1); break;
      case 'cyclePresetForward':   cyclePresets(1); break;
      case 'cyclePresetBack':      cyclePresets(-1); break;
      default:
        // Per-preset jump combos are stored under the dynamic action key
        // `selectPreset:<name>`. Resolve within the current ecosystem.
        if (action.startsWith('selectPreset:')) {
          handlePresetSelect(action.slice('selectPreset:'.length));
        }
        break;
    }
  }

  /** Cycle through presets in the current ecosystem. dir=+1 forward, -1 back.
   *  Wraps. No-op when the ecosystem has zero presets. With nothing selected,
   *  forward picks the first / back picks the last. */
  function cyclePresets(dir: 1 | -1) {
    if (filteredPresets.length === 0) return;
    const current = $presetSelectionStore[currentEcosystem];
    const idx = current
      ? filteredPresets.findIndex(p => p.name === current)
      : -1;
    let next: number;
    if (idx === -1) {
      next = dir === 1 ? 0 : filteredPresets.length - 1;
    } else {
      next = (idx + dir + filteredPresets.length) % filteredPresets.length;
    }
    handlePresetSelect(filteredPresets[next].name);
  }

  /** Adjust one bound of the frequency range source. Linked mode only.
   *  Bounds clamped to [1, 200] Hz. */
  function adjustFrequencyRangeBound(channel: string, bound: 'min' | 'max', delta: number) {
    const store = channel === 'A' ? channelA : channelB;
    store.update(s => {
      const source = s.frequencySource;
      if (source?.type !== 'linked') return s;
      const min = source.rangeMin ?? 1;
      const max = source.rangeMax ?? 200;
      let newMin = min;
      let newMax = max;
      if (bound === 'min') {
        newMin = Math.max(1, Math.min(max, min + delta));
      } else {
        newMax = Math.min(200, Math.max(min, max + delta));
      }
      return { ...s, frequencySource: { ...source, rangeMin: newMin, rangeMax: newMax } };
    });
  }

  /** Adjust just one bound (rangeMin or rangeMax) of the intensity source.
   *  Only meaningful in linked mode — for static mode we no-op so the user
   *  can still bind the actions safely. Bounds clamped to [0, 200]. */
  function adjustIntensityRangeBound(channel: string, bound: 'min' | 'max', delta: number) {
    const store = channel === 'A' ? channelA : channelB;
    store.update(s => {
      const source = s.intensitySource;
      if (source?.type !== 'linked') return s;
      const min = source.rangeMin ?? 0;
      const max = source.rangeMax ?? 200;
      let newMin = min;
      let newMax = max;
      if (bound === 'min') {
        newMin = Math.max(0, Math.min(max, min + delta));
      } else {
        newMax = Math.min(200, Math.max(min, max + delta));
      }
      return {
        ...s,
        rangeMin: newMin,
        rangeMax: newMax,
        intensitySource: { ...source, rangeMin: newMin, rangeMax: newMax },
      };
    });
  }

  // Keyboard shortcuts: map e.key → action name → dispatch
  function handleKeydown(e: KeyboardEvent) {
    // Don't handle shortcuts when typing in inputs OR rebinding
    if (e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement) return;
    if (rebindCapture) return;

    const k = e.key;

    // Help requires shift modifier
    if (k === shortcuts.help && e.shiftKey) { dispatchAction('help'); return; }
    if (k === shortcuts.settings) { dispatchAction('settings'); return; }
    if (k === shortcuts.toggleOutputPause) {
      e.preventDefault(); // Prevent space from scrolling
      dispatchAction('toggleOutputPause');
      return;
    }
    // Preset cycle bindings — empty string means unbound, never matches.
    if (shortcuts.cyclePresetForward && k === shortcuts.cyclePresetForward) {
      dispatchAction('cyclePresetForward');
      return;
    }
    if (shortcuts.cyclePresetBack && k === shortcuts.cyclePresetBack) {
      dispatchAction('cyclePresetBack');
      return;
    }
    if (shortcuts.cyclePreset && k === shortcuts.cyclePreset) {
      dispatchAction('cyclePreset');
      return;
    }

    // Channel adjustments — first match wins
    const map: [string, string][] = [
      [shortcuts.channelAFreqUp, 'channelAFreqUp'],
      [shortcuts.channelAFreqDown, 'channelAFreqDown'],
      [shortcuts.channelAIntUp, 'channelAIntUp'],
      [shortcuts.channelAIntDown, 'channelAIntDown'],
      [shortcuts.channelAFreqBalUp, 'channelAFreqBalUp'],
      [shortcuts.channelAFreqBalDown, 'channelAFreqBalDown'],
      [shortcuts.channelAIntBalUp, 'channelAIntBalUp'],
      [shortcuts.channelAIntBalDown, 'channelAIntBalDown'],
      [shortcuts.channelBFreqUp, 'channelBFreqUp'],
      [shortcuts.channelBFreqDown, 'channelBFreqDown'],
      [shortcuts.channelBIntUp, 'channelBIntUp'],
      [shortcuts.channelBIntDown, 'channelBIntDown'],
      [shortcuts.channelBFreqBalUp, 'channelBFreqBalUp'],
      [shortcuts.channelBFreqBalDown, 'channelBFreqBalDown'],
      [shortcuts.channelBIntBalUp, 'channelBIntBalUp'],
      [shortcuts.channelBIntBalDown, 'channelBIntBalDown'],
    ];
    for (const [bound, action] of map) {
      if (k === bound) { dispatchAction(action); return; }
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

  // Fractional period change accumulators — let continuous axis input
  // produce sub-tick frequency adjustments without losing precision.
  let _periodAccum: Record<string, number> = { A: 0, B: 0 };

  function adjustFrequency(channel: string, direction: 'up' | 'down', magnitude: number = 1) {
    const store = channel === 'A' ? channelA : channelB;
    store.update(s => {
      const source = s.frequencySource;
      const sign = direction === 'up' ? 1 : -1;

      // If linked mode, adjust the range. Range delta of ±5 scaled by magnitude.
      if (source?.type === 'linked') {
        const currentMin = source.rangeMin ?? 1;
        const currentMax = source.rangeMax ?? 200;
        const rangeSize = currentMax - currentMin;
        const delta = sign * 5 * magnitude;

        let newMin = currentMin + delta;
        let newMax = currentMax + delta;

        if (newMin < 1) { newMin = 1; newMax = 1 + rangeSize; }
        if (newMax > 200) { newMax = 200; newMin = Math.max(1, 200 - rangeSize); }

        return { ...s, frequencySource: { ...source, rangeMin: newMin, rangeMax: newMax } };
      }

      // Static mode: accumulate fractional period change so low magnitudes
      // still produce occasional integer steps over time.
      _periodAccum[channel] += sign * magnitude;
      const step = Math.trunc(_periodAccum[channel]);
      if (step === 0) return s;
      _periodAccum[channel] -= step;

      const currentPeriod = Math.round(1000 / s.frequency);
      // up = shorter period = higher frequency
      const newPeriod = currentPeriod - step;
      const clampedPeriod = Math.max(5, Math.min(1000, newPeriod));
      const newFrequency = 1000 / clampedPeriod;

      const updatedSource = source ? { ...source, staticValue: newFrequency } : undefined;
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
    // Spread each source first, then backfill defaults. The spread keeps
    // every optional field (midpoint, delayMs, transforms, ...) so a saved
    // preset captures the full link config — matching the runtime persist
    // path in buildChannelSettingsPayload.
    return {
      channelA: {
        frequencySource: {
          ...$channelA.frequencySource,
          type: $channelA.frequencySource?.type ?? 'static',
          staticValue: $channelA.frequencySource?.staticValue ?? $channelA.frequency,
          sourceAxis: $channelA.frequencySource?.sourceAxis ?? 'L0',
          rangeMin: $channelA.frequencySource?.rangeMin ?? 1,
          rangeMax: $channelA.frequencySource?.rangeMax ?? 200,
          curve: $channelA.frequencySource?.curve ?? 'linear',
          curveStrength: $channelA.frequencySource?.curveStrength ?? 2.0
        },
        frequencyBalanceSource: {
          ...$channelA.frequencyBalanceSource,
          type: $channelA.frequencyBalanceSource?.type ?? 'static',
          staticValue: $channelA.frequencyBalanceSource?.staticValue ?? $channelA.frequencyBalance,
          sourceAxis: $channelA.frequencyBalanceSource?.sourceAxis ?? 'L0',
          rangeMin: $channelA.frequencyBalanceSource?.rangeMin ?? 0,
          rangeMax: $channelA.frequencyBalanceSource?.rangeMax ?? 255,
          curve: $channelA.frequencyBalanceSource?.curve ?? 'linear',
          curveStrength: $channelA.frequencyBalanceSource?.curveStrength ?? 2.0
        },
        intensityBalanceSource: {
          ...$channelA.intensityBalanceSource,
          type: $channelA.intensityBalanceSource?.type ?? 'static',
          staticValue: $channelA.intensityBalanceSource?.staticValue ?? $channelA.intensityBalance,
          sourceAxis: $channelA.intensityBalanceSource?.sourceAxis ?? 'L0',
          rangeMin: $channelA.intensityBalanceSource?.rangeMin ?? 0,
          rangeMax: $channelA.intensityBalanceSource?.rangeMax ?? 255,
          curve: $channelA.intensityBalanceSource?.curve ?? 'linear',
          curveStrength: $channelA.intensityBalanceSource?.curveStrength ?? 2.0
        },
        intensitySource: {
          ...$channelA.intensitySource,
          type: $channelA.intensitySource?.type ?? 'linked',
          staticValue: $channelA.intensitySource?.staticValue ?? 100,
          sourceAxis: $channelA.intensitySource?.sourceAxis ?? 'L0',
          rangeMin: $channelA.intensitySource?.rangeMin ?? $channelA.rangeMin,
          rangeMax: $channelA.intensitySource?.rangeMax ?? $channelA.rangeMax,
          curve: $channelA.intensitySource?.curve ?? 'linear',
          curveStrength: $channelA.intensitySource?.curveStrength ?? 2.0
        }
      },
      channelB: {
        frequencySource: {
          ...$channelB.frequencySource,
          type: $channelB.frequencySource?.type ?? 'static',
          staticValue: $channelB.frequencySource?.staticValue ?? $channelB.frequency,
          sourceAxis: $channelB.frequencySource?.sourceAxis ?? 'R2',
          rangeMin: $channelB.frequencySource?.rangeMin ?? 1,
          rangeMax: $channelB.frequencySource?.rangeMax ?? 200,
          curve: $channelB.frequencySource?.curve ?? 'linear',
          curveStrength: $channelB.frequencySource?.curveStrength ?? 2.0
        },
        frequencyBalanceSource: {
          ...$channelB.frequencyBalanceSource,
          type: $channelB.frequencyBalanceSource?.type ?? 'static',
          staticValue: $channelB.frequencyBalanceSource?.staticValue ?? $channelB.frequencyBalance,
          sourceAxis: $channelB.frequencyBalanceSource?.sourceAxis ?? 'R2',
          rangeMin: $channelB.frequencyBalanceSource?.rangeMin ?? 0,
          rangeMax: $channelB.frequencyBalanceSource?.rangeMax ?? 255,
          curve: $channelB.frequencyBalanceSource?.curve ?? 'linear',
          curveStrength: $channelB.frequencyBalanceSource?.curveStrength ?? 2.0
        },
        intensityBalanceSource: {
          ...$channelB.intensityBalanceSource,
          type: $channelB.intensityBalanceSource?.type ?? 'static',
          staticValue: $channelB.intensityBalanceSource?.staticValue ?? $channelB.intensityBalance,
          sourceAxis: $channelB.intensityBalanceSource?.sourceAxis ?? 'R2',
          rangeMin: $channelB.intensityBalanceSource?.rangeMin ?? 0,
          rangeMax: $channelB.intensityBalanceSource?.rangeMax ?? 255,
          curve: $channelB.intensityBalanceSource?.curve ?? 'linear',
          curveStrength: $channelB.intensityBalanceSource?.curveStrength ?? 2.0
        },
        intensitySource: {
          ...$channelB.intensitySource,
          type: $channelB.intensitySource?.type ?? 'linked',
          staticValue: $channelB.intensitySource?.staticValue ?? 100,
          sourceAxis: $channelB.intensitySource?.sourceAxis ?? 'R2',
          rangeMin: $channelB.intensitySource?.rangeMin ?? $channelB.rangeMin,
          rangeMax: $channelB.intensitySource?.rangeMax ?? $channelB.rangeMax,
          curve: $channelB.intensitySource?.curve ?? 'linear',
          curveStrength: $channelB.intensitySource?.curveStrength ?? 2.0
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
      frequencySource: { ...aFreq, curve: aFreq.curve as any },
      frequencyBalanceSource: { ...aFreqBal, curve: aFreqBal.curve as any },
      intensityBalanceSource: { ...aIntBal, curve: aIntBal.curve as any },
      intensitySource: { ...aInt, curve: aInt.curve as any }
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
      frequencySource: { ...bFreq, curve: bFreq.curve as any },
      frequencyBalanceSource: { ...bFreqBal, curve: bFreqBal.curve as any },
      intensityBalanceSource: { ...bIntBal, curve: bIntBal.curve as any },
      intensitySource: { ...bInt, curve: bInt.curve as any }
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

  // Reorder dialog state. Opens a centered modal with a drag-and-drop list
  // (svelte-dnd-action). Local working copy lets the user reorder freely;
  // persistence happens on `finalize` via reorder_presets.
  //
  // svelte-dnd-action requires every item to carry an `id` property, so the
  // working copy tags each preset with `id: name`. Without it the library
  // throws "missing 'id' property", and that async rejection intermittently
  // aborts Svelte's flush when the dialog closes — leaving the modal stuck
  // open (Done worked, but the X/Escape/overlay close paths did not).
  type ReorderItem = ChannelPreset & { id: string };
  let reorderOpen = $state(false);
  let reorderItems: ReorderItem[] = $state([]);
  // Per-row delete-confirm popover state, keyed by preset id (== name).
  let deleteConfirmOpen: Record<string, boolean> = $state({});
  const REORDER_FLIP_MS = 200;

  // Gamepad jump-to-preset combos live in the shared gamepadBindings map under
  // the key `selectPreset:<name>`, so they persist and evaluate through the same
  // pipeline as every other binding. Scoped to the current ecosystem at dispatch.
  const presetBindKey = (name: string) => `selectPreset:${name}`;

  function openReorderDialog() {
    reorderItems = filteredPresets.map(p => ({ ...p, id: p.name }));
    deleteConfirmOpen = {};
    reorderOpen = true;
  }

  // Abandon an in-progress preset-combo capture if the modal closes, so it
  // doesn't linger and hijack the next gamepad press elsewhere.
  $effect(() => {
    if (!reorderOpen && rebindCapture?.action.startsWith('selectPreset:')) {
      cancelRebind();
    }
  });

  function handleReorderConsider(e: CustomEvent<DndEvent<ReorderItem>>) {
    reorderItems = e.detail.items;
  }

  async function handleReorderFinalize(e: CustomEvent<DndEvent<ReorderItem>>) {
    reorderItems = e.detail.items;
    const names = reorderItems.map(p => p.name);
    try {
      await invoke('reorder_presets', { names });
      presets = await invoke<ChannelPreset[]>('get_presets');
    } catch (err) {
      console.error('[Presets] Failed to reorder:', err);
    }
  }

  async function deletePresetFromReorder(item: ReorderItem) {
    deleteConfirmOpen[item.id] = false;
    try {
      await invoke('delete_preset', { name: item.name });
      // Drop the selection if the deleted preset was active.
      if ($presetSelectionStore[currentEcosystem] === item.name) {
        clearSelectedPreset(currentEcosystem);
        lastSavedPresetState = null;
        presetDirty = false;
      }
      presets = await invoke<ChannelPreset[]>('get_presets');
      reorderItems = reorderItems.filter(p => p.id !== item.id);
      // Drop any gamepad jump-combo bound to the deleted preset.
      const bindKey = presetBindKey(item.name);
      if (gamepadBindings[bindKey]) {
        const next = { ...gamepadBindings };
        delete next[bindKey];
        gamepadBindings = next;
      }
    } catch (err) {
      console.error('[Presets] Failed to delete:', err);
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
  $effect(() => {
    if (settingsLoaded && selectedPresetName && lastSavedPresetState && $channelA && $channelB) {
      presetDirty = checkPresetDirty();
    }
  });

</script>

<svelte:window onkeydown={handleKeydown} />

<main class="h-screen w-full bg-background text-foreground overflow-hidden flex flex-col">
  <!-- Compact Header -->
  <header class="border-b border-border bg-card/50 backdrop-blur-sm w-full">
    <div class="w-full px-4 py-3 max-w-[1920px] mx-auto">
      <div class="flex items-center justify-between">
        <div class="flex items-center gap-3">
          <Zap class="h-6 w-6 text-primary" />
          <h1 class="text-xl font-bold bg-linear-to-r from-primary to-secondary bg-clip-text text-transparent">
            CoyoteSocket
          </h1>
        </div>

        <div class="flex items-center gap-2">
          <!-- Gamepad connection status (icon-only) -->
          <GamepadStatusPill />

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
              onclick={toggleOutputPause}
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
            onclick={() => settingsOpen = true}
            class="h-8 w-8"
          >
            <Settings class="h-4 w-4" />
          </Button>

          <Button
            variant="ghost"
            size="icon"
            onclick={() => helpOpen = true}
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
                  class="flex-1 py-1 px-2 text-xs bg-transparent border-none outline-hidden min-w-0 text-foreground placeholder:text-muted-foreground"
                  onkeydown={(e) => e.key === 'Enter' && saveNewPreset()}
                />
                <button
                  onclick={saveNewPreset}
                  disabled={!newPresetName.trim()}
                  class="w-7 shrink-0 flex items-center justify-center bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50 border-l border-border"
                  title="Save preset"
                >
                  <Save class="h-3.5 w-3.5" />
                </button>
                <button
                  onclick={cancelAddingPreset}
                  class="w-7 shrink-0 flex items-center justify-center hover:bg-background/50 border-l border-border"
                  title="Cancel"
                >
                  <X class="h-3.5 w-3.5" />
                </button>
              {:else}
                <select
                  class="preset-select flex-1 py-1 pl-2 pr-1 mr-1 text-xs bg-transparent border-none outline-hidden min-w-0 cursor-pointer"
                  value={selectedPresetName}
                  onchange={(e) => handlePresetSelect(e.currentTarget.value)}
                >
                  <option value="">None</option>
                  {#each filteredPresets as preset}
                    <option value={preset.name}>{preset.name}</option>
                  {/each}
                </select>
                <button
                  onclick={startAddingPreset}
                  class="w-7 shrink-0 flex items-center justify-center hover:bg-background/50 border-l border-border"
                  title="Save current settings as new preset"
                >
                  <Plus class="h-3.5 w-3.5" />
                </button>
                <button
                  onclick={openReorderDialog}
                  class="w-7 shrink-0 flex items-center justify-center hover:bg-background/50 border-l border-border"
                  title="Reorder presets"
                >
                  <ListOrdered class="h-3.5 w-3.5" />
                </button>
                {#if presetDirty && selectedPresetName}
                  <button
                    onclick={saveCurrentPreset}
                    class="w-7 shrink-0 flex items-center justify-center bg-primary text-primary-foreground hover:bg-primary/90 border-l border-border"
                    title="Save changes to '{selectedPresetName}'"
                  >
                    <Save class="h-3.5 w-3.5" />
                  </button>
                {/if}
              {/if}
            </div>
          </div>

          {#if $currentInputSource === 'tcode' || $currentInputSource === 'none'}
            <!-- Engine selector (T-Code only). Native dropdown for the pick;
                 a click-only Info popover next to it shows the description
                 for the currently-selected engine. -->
            <div class="flex h-7 rounded border border-border overflow-hidden bg-background/50">
              <span class="text-xs text-muted-foreground px-2 flex items-center border-r border-border bg-muted/30 whitespace-nowrap">
                Engine
              </span>
              <select
                class="preset-select py-1 pl-2 pr-1 mr-1 text-xs bg-transparent border-none outline-hidden cursor-pointer"
                bind:value={$generalSettings.processingEngine}
              >
                {#each PROCESSING_ENGINES as engine}
                  <option value={engine.value}>{engine.label}</option>
                {/each}
              </select>
              <Popover bind:open={engineInfoOpen} compact={true} contentClass="w-[260px]! min-w-0">
                {#snippet trigger()}
                                <button
                    
                    type="button"
                    class="w-7 h-7 shrink-0 flex items-center justify-center text-muted-foreground hover:text-foreground hover:bg-background/50 border-l border-border"
                    title="About this engine"
                  >
                    <Info class="h-3.5 w-3.5" />
                  </button>
                              {/snippet}
                <div class="space-y-1">
                  <div class="text-xs font-medium">{activeEngine?.label ?? ''}</div>
                  <div class="text-[11px] leading-snug text-muted-foreground">
                    {activeEngine?.description ?? ''}
                  </div>
                </div>
              </Popover>
            </div>

            <!-- Peak-fill variant selector. Only meaningful for V2 Detailed,
                 so hide entirely for other engines instead of showing a
                 disabled-and-faded control. -->
            {#if $generalSettings.processingEngine === 'v2-detailed'}
              <div class="flex h-7 pr-1 rounded border border-border overflow-hidden bg-background/50">
                <Tooltip content="V2 Detailed peak-fill variant. v1 (Legacy) back-fills empty buckets from previous values. v2 (Forward-fill) prefers the next sample for stronger peak preservation.">
                  <span class="text-[11px] text-muted-foreground px-2 flex items-center gap-1 border-r border-border bg-muted/30 cursor-help whitespace-nowrap">
                    Peak Fill
                    <Info class="h-3 w-3" />
                  </span>
                </Tooltip>
                <select
                  class="preset-select py-1 pl-2 pr-2 text-xs bg-transparent border-none outline-hidden cursor-pointer"
                  bind:value={$generalSettings.peakFill}
                >
                  {#each PEAK_FILL_STRATEGIES as strat}
                    <option value={strat.value}>{strat.label}</option>
                  {/each}
                </select>
              </div>
            {/if}
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
      { value: 'shortcuts', label: 'Keyboard Shortcuts' },
      { value: 'gamepad', label: 'Gamepad' }
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
      {:else if settingsTab === 'gamepad'}
        <div class="space-y-3">
          <p class="text-xs text-muted-foreground">
            Bind any action to a controller button or stick/trigger direction. Gamepad input works regardless of window focus.
          </p>

          <div class="flex items-center justify-between gap-2 pb-2 border-b">
            <label for="gamepad-engine" class="text-xs">Input Engine</label>
            <select
              id="gamepad-engine"
              class="px-2 py-1 text-xs bg-muted rounded"
              value={gamepadEngine}
              onchange={onGamepadEngineChange}
            >
              <option value="xinput">XInput (Xbox-only, recommended on Windows)</option>
              <option value="gilrs">gilrs (Xbox + Switch + generic, cross-platform)</option>
              <option value="off">Off</option>
            </select>
          </div>

          <div class="flex items-center justify-between gap-3 pb-2 border-b">
            <div class="flex flex-col flex-1">
              <label for="stick-sensitivity" class="text-xs">Stick Sensitivity</label>
              <span class="text-[10px] text-muted-foreground">
                Multiplier for action-driven stick/trigger adjustments. Direct parameter linking ignores this.
              </span>
            </div>
            <span class="text-xs font-mono w-10 text-right">{gamepadStickSensitivity.toFixed(2)}</span>
            <input
              id="stick-sensitivity"
              type="range"
              min="0.1"
              max="3.0"
              step="0.05"
              value={gamepadStickSensitivity}
              oninput={onSensitivityInput}
              class="w-32"
            />
          </div>

          <div class="flex items-center justify-between gap-3 pb-2 border-b">
            <div class="flex flex-col flex-1">
              <label for="repeat-delay" class="text-xs">Repeat Delay</label>
              <span class="text-[10px] text-muted-foreground">
                How long to hold a button-only binding before auto-repeat kicks in.
              </span>
            </div>
            <span class="text-xs font-mono w-14 text-right">{Math.round(gamepadRepeatDelay)}ms</span>
            <input
              id="repeat-delay"
              type="range"
              min="50"
              max="2000"
              step="10"
              value={gamepadRepeatDelay}
              oninput={onRepeatDelayInput}
              class="w-32"
            />
          </div>

          <div class="flex items-center justify-between gap-3 pb-2 border-b">
            <div class="flex flex-col flex-1">
              <label for="repeat-interval" class="text-xs">Repeat Interval</label>
              <span class="text-[10px] text-muted-foreground">
                Time between auto-repeat fires once it kicks in. Lower = faster.
              </span>
            </div>
            <span class="text-xs font-mono w-14 text-right">{Math.round(gamepadRepeatInterval)}ms</span>
            <input
              id="repeat-interval"
              type="range"
              min="20"
              max="1000"
              step="10"
              value={gamepadRepeatInterval}
              oninput={onRepeatIntervalInput}
              class="w-32"
            />
          </div>

          {#each ['A', 'B', 'global'] as group}
            <div class="space-y-1">
              <h4 class="text-sm font-medium {group === 'A' ? 'text-primary' : group === 'B' ? 'text-secondary' : 'text-muted-foreground'}">
                {group === 'A' ? 'Channel A' : group === 'B' ? 'Channel B' : 'Global'}
              </h4>
              <div class="space-y-1">
                {#each ACTION_ROWS.filter(r => r.group === group) as row}
                  {@const binding = gamepadBindings[row.action]}
                  {@const capture = rebindCapture?.action === row.action && rebindCapture?.source === 'gamepad' ? rebindCapture : null}
                  {@const kbKey = shortcutKeyFor(row.action)}
                  <div class="flex items-center justify-between gap-2 text-xs py-1">
                    <span class="flex-1">{row.label}</span>
                    <span class="w-10 text-center text-muted-foreground font-mono">
                      {kbKey === ' ' ? 'Space' : (kbKey ?? '')}
                    </span>
                    <GamepadBindControl
                      {binding}
                      capturing={!!capture}
                      captureParts={capture?.parts ?? []}
                      on:start={() => startRebindGamepad(row.action)}
                      on:save={saveCombo}
                      on:cancel={cancelRebind}
                      on:clear={() => clearGamepadBinding(row.action)}
                    />
                  </div>
                {/each}
              </div>
            </div>
          {/each}
        </div>
      {/if}
    </TabsClassic>

    <div class="flex justify-end gap-2 pt-3 border-t shrink-0">
      <Button variant="outline" size="sm" onclick={() => settingsOpen = false}>
        Cancel
      </Button>
      <Button
        size="sm"
        class="save-button"
        onclick={() => {
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

  <!-- Reorder Presets Modal -->
  <Dialog bind:open={reorderOpen} title="Reorder Presets ({currentEcosystem})">
    <div class="text-xs text-muted-foreground mb-2">
      Drag rows to reorder. Saves automatically.
    </div>
    {#if reorderItems.length === 0}
      <div class="text-xs text-muted-foreground py-4 text-center">
        No presets in this ecosystem.
      </div>
    {:else}
      <div
        class="flex flex-col gap-1 overflow-y-auto scrollbar-thin pr-1 max-h-[60vh]"
        use:dndzone={{ items: reorderItems, flipDurationMs: REORDER_FLIP_MS, dropTargetStyle: {} }}
        onconsider={handleReorderConsider}
        onfinalize={handleReorderFinalize}
      >
        {#each reorderItems as preset (preset.id)}
          {@const bindKey = presetBindKey(preset.name)}
          {@const binding = gamepadBindings[bindKey]}
          {@const capture = rebindCapture?.action === bindKey && rebindCapture?.source === 'gamepad' ? rebindCapture : null}
          <div
            animate:flip={{ duration: REORDER_FLIP_MS }}
            class="flex items-center gap-2 px-2 py-2 rounded border border-border bg-muted/30 cursor-grab active:cursor-grabbing"
          >
            <GripVertical class="h-4 w-4 text-muted-foreground shrink-0" />
            <span class="text-sm flex-1 truncate">{preset.name}</span>
            <!-- Gamepad jump-combo: inline capture, mirrors the settings
                 rebind UI. Combo is always visible; click to capture in place.
                 stopPropagation wrapper so interacting never starts a drag. -->
            <div
              class="shrink-0 flex items-center gap-1"
              onmousedown={(e) => e.stopPropagation()}
              ontouchstart={(e) => e.stopPropagation()}
              onpointerdown={(e) => e.stopPropagation()}
              role="presentation"
            >
              <GamepadBindControl
                compact
                {binding}
                capturing={!!capture}
                captureParts={capture?.parts ?? []}
                label={`jump to “${preset.name}”`}
                on:start={() => startRebindGamepad(bindKey)}
                on:save={saveCombo}
                on:cancel={cancelRebind}
                on:clear={() => clearGamepadBinding(bindKey)}
              />
            </div>
            <!-- Delete control (hidden while this row is capturing to save room).
                 Stop pointer/touch events from reaching the dndzone so
                 interacting with it never starts a drag. -->
            {#if !capture}
            <div
              class="shrink-0"
              onmousedown={(e) => e.stopPropagation()}
              ontouchstart={(e) => e.stopPropagation()}
              onpointerdown={(e) => e.stopPropagation()}
              role="presentation"
            >
              <Popover bind:open={deleteConfirmOpen[preset.id]} align="end" compact={true} contentClass="w-[220px]! min-w-0">
                {#snippet trigger()}
                                    <button
                    
                    type="button"
                    class="w-7 h-7 flex items-center justify-center rounded text-muted-foreground hover:text-destructive hover:bg-destructive/10 cursor-pointer"
                    title="Delete preset"
                  >
                    <Trash2 class="h-4 w-4" />
                  </button>
                                  {/snippet}
                <div class="space-y-2">
                  <div class="text-xs leading-snug">
                    Delete preset <span class="font-medium">{preset.name}</span>?
                  </div>
                  <div class="flex justify-end gap-2">
                    <Button
                      variant="ghost"
                      class="h-7 px-2 text-xs"
                      onclick={() => (deleteConfirmOpen[preset.id] = false)}
                    >
                      Cancel
                    </Button>
                    <Button
                      variant="destructive"
                      class="h-7 px-2 text-xs"
                      onclick={() => deletePresetFromReorder(preset)}
                    >
                      Delete
                    </Button>
                  </div>
                </div>
              </Popover>
            </div>
            {/if}
          </div>
        {/each}
      </div>
    {/if}
    <div class="flex justify-end mt-3 pt-3 border-t border-border">
      <Button onclick={() => (reorderOpen = false)}>Done</Button>
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