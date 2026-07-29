<!--
  The QR the phone scans, and the URL it points at.

  Rendered as an SVG by the Rust side and injected here, so the same code
  produces the QR in the window, on `/pair`, and behind the tray menu item —
  three places that must not be able to disagree about the address.
-->
<script lang="ts">
  import { bridge } from './bridge.svelte'

  const urls = $derived(bridge.status?.urls)
  const httpError = $derived(bridge.status?.httpError ?? null)
</script>

<section class="panel">
  <h2>Phone</h2>

  {#if httpError}
    <p class="bad small">
      The phone-facing server did not start, so this QR leads nowhere.
      <span class="mono">{httpError}</span>
    </p>
  {/if}

  {#if bridge.qrSvg}
    <!-- The SVG comes from our own qr.rs, not from user input. -->
    <div class="qr">{@html bridge.qrSvg}</div>
  {/if}

  <p class="url mono">{urls?.pairing ?? '—'}</p>
  <div class="row">
    <button onclick={() => urls && bridge.open(`${urls.local}/pair`)}>
      Open pairing page
    </button>
    <button onclick={() => urls && bridge.open(`${urls.local}/healthz`)}>
      Status JSON
    </button>
  </div>
  <p class="small muted">
    Both devices must be on the same network. The phone side is plain HTTP, so
    Web Bluetooth will not work there yet — that needs HTTPS, and it is not
    built.
  </p>
</section>

<style>
  .qr {
    background: #fff;
    border-radius: 8px;
    padding: 0.5rem;
    width: min(100%, 12rem);
    margin: 0 auto 0.6rem;
  }

  .qr :global(svg) {
    display: block;
    width: 100%;
    height: auto;
  }

  .url {
    text-align: center;
    font-size: 0.85rem;
    overflow-wrap: anywhere;
    margin: 0 0 0.6rem;
  }

  .bad {
    color: var(--bad);
  }
</style>
