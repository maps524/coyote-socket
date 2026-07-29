<!--
  Endpoint, Connect/Disconnect, and an honest account of what went wrong.

  The fault block is the point of this panel. A bare "could not connect" would
  make the headset test worthless: the whole reason to run it is to find out
  whether our reading of the protocol is right, and that is only knowable once
  every other explanation has been ruled out and named.
-->
<script lang="ts">
  import { bridge } from './bridge.svelte'
  import StatusPill from './StatusPill.svelte'
  import type { Reachability } from './types'

  let endpoint = $state('')
  let seeded = false

  // Seed the field from settings once, then leave it alone — re-syncing it on
  // every status refresh would fight with someone typing.
  $effect(() => {
    const remembered = bridge.status?.endpoint
    if (!seeded && remembered) {
      endpoint = remembered
      seeded = true
    }
  })

  const link = $derived(bridge.snapshot.link)
  const connected = $derived(link === 'connected')
  const trying = $derived(link === 'connecting' || link === 'retrying')
  const fault = $derived(bridge.snapshot.fault)

  const probeText: Record<Reachability['outcome'], string> = {
    'port-open': 'Port is open — something is listening.',
    'port-closed':
      'The host answered and refused the port. That is what remote control being switched off looks like.',
    'host-up-port-filtered':
      'The host is there, but the port did not answer. Something is filtering it.',
    'no-response': 'Nothing at that address answered.',
    'bad-address': 'That is not an address we can dial.',
  }

  function submit(event: SubmitEvent) {
    event.preventDefault()
    if (connected || trying) bridge.disconnect()
    else bridge.connect(endpoint)
  }
</script>

<section class="panel">
  <div class="head">
    <h2>Player</h2>
    <StatusPill {link} attempts={bridge.snapshot.attempts} />
  </div>

  <form onsubmit={submit}>
    <input
      class="mono"
      bind:value={endpoint}
      placeholder="192.168.1.50"
      list="recent-endpoints"
      aria-label="Player address"
      disabled={connected || trying}
    />
    <datalist id="recent-endpoints">
      {#each bridge.status?.recents ?? [] as recent (recent)}
        <option value={recent}></option>
      {/each}
    </datalist>

    <button
      type="button"
      onclick={() => bridge.check(endpoint)}
      disabled={bridge.busy || !endpoint}
      title="Check the address without connecting"
    >
      Test
    </button>
    <button type="submit" class="primary" disabled={bridge.busy || !endpoint}>
      {connected || trying ? 'Disconnect' : 'Connect'}
    </button>
  </form>

  <p class="small muted hint">
    Port defaults to 23554. Remote control has to be switched on in DeoVR or
    HereSphere first — neither opens the port until it is.
  </p>

  {#if bridge.probe && bridge.probe.outcome !== 'port-open'}
    <p class="note warn">{probeText[bridge.probe.outcome]}</p>
  {/if}

  {#if fault}
    <div class="fault {fault.kind}">
      <strong>{fault.kind.replace('-', ' ')}</strong>
      {#if fault.hint}
        <p>{fault.hint}</p>
      {/if}
      <!-- Verbatim, under the explanation. If our explanation is wrong, this
           is the thing that reveals it. -->
      <p class="mono small detail">{fault.detail}</p>
    </div>
  {/if}

  {#if bridge.error}
    <p class="note bad">{bridge.error}</p>
  {/if}
</section>

<style>
  .head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 0.7rem;
  }

  .head h2 {
    margin: 0;
  }

  form {
    display: flex;
    gap: 0.5rem;
  }

  input {
    flex: 1;
    min-width: 8rem;
  }

  .hint {
    margin: 0.5rem 0 0;
  }

  .note {
    margin: 0.6rem 0 0;
    font-size: 0.85rem;
  }

  .warn {
    color: var(--warn);
  }

  .bad {
    color: var(--bad);
  }

  .fault {
    margin-top: 0.7rem;
    padding: 0.6rem 0.75rem;
    border-radius: 8px;
    border-left: 3px solid var(--warn);
    background: var(--panel-2);
  }

  .fault strong {
    text-transform: capitalize;
    color: var(--warn);
  }

  /* Framing is the interesting failure, so it does not share a colour with
     "the headset is asleep". */
  .fault.framing {
    border-left-color: var(--bad);
  }

  .fault.framing strong {
    color: var(--bad);
  }

  .fault p {
    margin: 0.4rem 0 0;
  }

  .detail {
    color: var(--muted);
    word-break: break-word;
  }
</style>
