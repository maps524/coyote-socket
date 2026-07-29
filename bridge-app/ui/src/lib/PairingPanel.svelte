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
  const pairingUrl = $derived(bridge.status?.pairingUrl ?? null)

  // The URL grants access, so it is not shown by default — a window on a desk
  // or in a screen share should not be a credential.
  let revealed = $state(false)
  let confirmingRevoke = $state(false)
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

  <!--
    The URL carries the pairing token, which is password-equivalent. Hidden by
    default so a window left open, or a screen share, does not hand it out —
    the QR is the intended channel and does not have to be readable to work.
  -->
  <p class="url mono">
    {#if revealed}
      {pairingUrl ?? '—'}
    {:else}
      {urls?.pairingBase ?? '—'}<span class="muted">?t=…</span>
    {/if}
  </p>
  <div class="row">
    <button onclick={() => (revealed = !revealed)}>
      {revealed ? 'Hide token' : 'Show token'}
    </button>
    <button onclick={() => urls && bridge.open(`${urls.local}/pair`)}>
      Open pairing page
    </button>
  </div>

  <p class="small muted">
    Both devices must be on the same network. The phone side is plain HTTP, so
    Web Bluetooth will not work there yet — that needs HTTPS, and it is not
    built.
  </p>

  <!--
    Revocation is a user action, not something pairing does on its own. There
    is one shared token, so this un-pairs everything at once — auto-rotating
    after a phone paired would silently break a tablet that paired yesterday.
    The confirm exists because that consequence is not guessable from the
    button.
  -->
  <div class="revoke">
    {#if confirmingRevoke}
      <p class="small warn">
        Every device that has paired — phone, tablet, any browser tab — stops
        working and has to scan the new QR. Continue?
      </p>
      <div class="row">
        <button onclick={() => (confirmingRevoke = false)}>Cancel</button>
        <button
          class="danger"
          onclick={async () => {
            confirmingRevoke = false
            await bridge.revokeToken()
          }}
        >
          Revoke and issue a new one
        </button>
      </div>
    {:else}
      <button class="small-btn" onclick={() => (confirmingRevoke = true)}>
        Revoke pairing token
      </button>
      <p class="small muted">
        Use if the QR was seen by someone else, or the URL was pasted somewhere
        it should not have been.
      </p>
    {/if}
  </div>
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

  .warn {
    color: var(--warn);
  }

  .revoke {
    margin-top: 0.9rem;
    padding-top: 0.7rem;
    border-top: 1px solid var(--line);
  }

  .revoke p {
    margin: 0.4rem 0 0;
  }

  .small-btn {
    font-size: 0.82rem;
    padding: 0.25rem 0.6rem;
  }

  .danger {
    border-color: var(--bad);
    color: var(--bad);
  }

  .danger:hover {
    background: var(--bad);
    color: #1a0505;
  }
</style>
