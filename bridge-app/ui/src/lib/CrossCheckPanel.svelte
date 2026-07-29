<!--
  The MultiFunPlayer cross-check.

  Our client and our fake player were written from the same reading of the same
  two documents, so every test we have is self-consistent by construction — a
  shared misreading passes all of them. MultiFunPlayer is an independent
  implementation of the same protocol, written by someone else, and it is
  already installed on this machine. Pointing it at our fake breaks the
  circularity without needing a headset.

  The panel is explicit about the limit of that: it validates our *server*
  framing against a third-party client. Our *client* against a real player is a
  separate question, and only a Quest answers it.
-->
<script lang="ts">
  import { bridge } from './bridge.svelte'

  const running = $derived(bridge.status?.fakePlayer ?? null)
  const fakeFrames = $derived(bridge.wire.filter((e) => e.source === 'fake'))
  const inbound = $derived(fakeFrames.filter((e) => e.dir === 'in' && e.kind !== 'event'))
  const connected = $derived(
    fakeFrames.some((e) => e.kind === 'event' && e.text?.includes('connected')),
  )
</script>

<section class="panel">
  <h2>Cross-check with MultiFunPlayer</h2>

  <div class="row">
    {#if running}
      <button onclick={() => bridge.stopFakePlayer()}>Stop test player</button>
      <span class="mono small">listening on {running}</span>
    {:else}
      <button class="primary" onclick={() => bridge.startFakePlayer()}>
        Run local test player
      </button>
      <span class="small muted">binds 0.0.0.0:23554</span>
    {/if}
  </div>

  {#if running}
    <ol class="steps small">
      <li>In MultiFunPlayer, open <strong>Settings → Media source</strong>.</li>
      <li>
        Enable <strong>DeoVR</strong> (or <strong>HereSphere</strong> — they use
        identical framing, so either exercises the same code).
      </li>
      <li>
        Set its endpoint to <code class="mono">127.0.0.1:23554</code>, or this
        machine's LAN address if MFP is on another box.
      </li>
      <li>Connect it. Watch the wire log below fill with <code>fake</code> rows.</li>
      <li>
        Scrub or play/pause in MFP. Its commands arrive as
        <code class="mono">fake ←</code> frames.
      </li>
    </ol>

    <div class="verdict" class:live={connected}>
      {#if !connected}
        Waiting for a client to connect.
      {:else if inbound.length === 0}
        Client connected, but has sent nothing yet.
      {:else}
        Client connected and has sent {inbound.length} frame{inbound.length === 1
          ? ''
          : 's'} we could read.
      {/if}
    </div>

    <p class="small muted">
      What this proves: a third-party client can read the frames we write, and
      we can read the frames it writes. What it does not prove: that a real
      DeoVR or HereSphere behaves the same. Only a headset settles that — but if
      MFP agrees with us here, a headset failure isolates to the real player
      rather than to our framing in general.
    </p>
  {/if}
</section>

<style>
  .steps {
    margin: 0.8rem 0;
    padding-left: 1.2rem;
    line-height: 1.7;
    color: var(--muted);
  }

  .steps strong,
  .steps code {
    color: var(--text);
  }

  .verdict {
    padding: 0.5rem 0.7rem;
    border-radius: 8px;
    background: var(--panel-2);
    border-left: 3px solid var(--line);
    font-size: 0.88rem;
  }

  .verdict.live {
    border-left-color: var(--ok);
  }
</style>
