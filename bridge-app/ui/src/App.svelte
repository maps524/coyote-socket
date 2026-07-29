<!--
  The bridge window.

  Layout follows the order of the test: connect, watch state arrive, read the
  wire. The banner at the top is not decoration — a window that renders
  "Connected" in green is an excellent way to forget that this client has never
  spoken to a real player, and forgetting that is how a spike gets mistaken for
  a finished feature.
-->
<script lang="ts">
  import { bridge } from './lib/bridge.svelte'
  import ConnectionPanel from './lib/ConnectionPanel.svelte'
  import CrossCheckPanel from './lib/CrossCheckPanel.svelte'
  import PairingPanel from './lib/PairingPanel.svelte'
  import PlayerPanel from './lib/PlayerPanel.svelte'
  import WireLog from './lib/WireLog.svelte'

  let ready = $state(false)
  let startupError = $state<string | null>(null)

  $effect(() => {
    bridge
      .init()
      .then(() => (ready = true))
      .catch((e) => (startupError = String(e)))
  })
</script>

<main>
  <header>
    <h1>CoyoteSocket Bridge</h1>
    <!--
      Kept deliberately precise. "Confirmed against a real DeoVR" without the
      qualifier would read as "this protocol is done", and it is not: one
      player, one version, one platform, and HereSphere entirely unobserved.
    -->
    <span class="small muted">
      spike {bridge.status?.version ?? ''} — confirmed against DeoVR on a Quest,
      once. HereSphere unobserved.
    </span>
  </header>

  {#if startupError}
    <p class="panel bad">Could not start: {startupError}</p>
  {:else if !ready}
    <p class="panel muted">Starting…</p>
  {:else}
    {#if bridge.framingFault}
      <!-- Sticky, and above everything. This is the result the spike was
           built to produce; it must not be something you have to scroll to. -->
      <div class="panel alarm">
        <strong>The bytes did not match our reading of the framing.</strong>
        <p class="small">
          This is a finding, not a glitch. Press <em>Copy all</em> in the wire
          log and send it back — it is the only evidence of what a real player
          actually does.
        </p>
      </div>
    {/if}

    <div class="grid">
      <div class="col">
        <ConnectionPanel />
        <PlayerPanel />
        <CrossCheckPanel />
      </div>
      <div class="col">
        <PairingPanel />
      </div>
    </div>

    <WireLog />
  {/if}
</main>

<style>
  main {
    display: flex;
    flex-direction: column;
    gap: 0.9rem;
    padding: 1rem;
    height: 100vh;
    max-width: 1200px;
    margin: 0 auto;
  }

  header {
    display: flex;
    align-items: baseline;
    gap: 0.8rem;
    flex-wrap: wrap;
  }

  h1 {
    font-size: 1.1rem;
    margin: 0;
  }

  .grid {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 16rem;
    gap: 0.9rem;
    align-items: start;
  }

  /* One column when the window is narrow; the QR is the thing that can wait. */
  @media (max-width: 820px) {
    .grid {
      grid-template-columns: minmax(0, 1fr);
    }
  }

  .col {
    display: flex;
    flex-direction: column;
    gap: 0.9rem;
    min-width: 0;
  }

  .alarm {
    border-color: var(--bad);
    border-left: 3px solid var(--bad);
  }

  .alarm strong {
    color: var(--bad);
  }

  .alarm p {
    margin: 0.3rem 0 0;
  }

  .bad {
    color: var(--bad);
  }

  /* The wire log takes whatever vertical room is left. */
  main > :global(section.panel.wire) {
    flex: 1;
    min-height: 14rem;
  }
</style>
