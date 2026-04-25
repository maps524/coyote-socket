<script lang="ts">
  import { Gamepad2 } from 'lucide-svelte';
  import { invoke } from '@tauri-apps/api/core';
  import Popover from './ui/Popover.svelte';
  import { gamepadStatus } from '$lib/stores/gamepadStatus';

  let popoverOpen = false;

  $: status = $gamepadStatus;

  type Engine = 'off' | 'gilrs' | 'xinput';
  const engines: { value: Engine; label: string }[] = [
    { value: 'off', label: 'Off' },
    { value: 'gilrs', label: 'Gilrs' },
    { value: 'xinput', label: 'XInput' },
  ];

  let switching = false;
  async function switchEngine(engine: Engine) {
    if (switching || status.engine === engine) return;
    switching = true;
    try {
      await invoke<string>('set_gamepad_engine', { engine });
    } catch (e) {
      console.error('[GamepadStatusPill] failed to switch engine:', e);
    } finally {
      switching = false;
    }
  }
</script>

<Popover bind:open={popoverOpen} align="start">
  <button
    slot="trigger"
    class="flex items-center gap-1.5 px-2.5 py-1 rounded-full text-xs font-medium transition-all
           {status.connected
             ? 'bg-green-500/20 text-green-400 border border-green-500/30 hover:bg-green-500/30'
             : 'bg-muted/50 text-muted-foreground border border-border hover:bg-muted'}"
  >
    <Gamepad2 class="h-4 w-4" />
    <span class="w-1.5 h-1.5 rounded-full {status.connected ? 'bg-green-400' : 'bg-muted-foreground/50'}"></span>
  </button>

  <div class="space-y-3 min-w-[200px]">
    <div class="flex items-center justify-between">
      <h3 class="text-sm font-medium">Gamepad</h3>
      <span class="text-xs {status.connected ? 'text-green-400' : 'text-muted-foreground'}">
        {status.connected ? `${status.count} Connected` : 'Disconnected'}
      </span>
    </div>

    <div class="space-y-1">
      <div class="text-[10px] text-muted-foreground uppercase tracking-wider">Engine</div>
      <div class="flex items-center gap-1">
        {#each engines as opt}
          <button
            type="button"
            class="flex-1 px-2 py-1 text-xs rounded border transition-colors
                   {status.engine === opt.value
                     ? 'bg-primary text-primary-foreground border-primary'
                     : 'bg-muted/30 text-muted-foreground border-border hover:bg-muted'}"
            disabled={switching}
            on:click={() => switchEngine(opt.value)}
          >
            {opt.label}
          </button>
        {/each}
      </div>
      <div class="text-[10px] text-muted-foreground/70">
        {status.engine === 'xinput'
          ? 'Windows-only, best for Xbox controllers.'
          : status.engine === 'gilrs'
            ? 'Cross-platform, supports most controllers.'
            : 'Gamepad input disabled.'}
      </div>
    </div>
  </div>
</Popover>
