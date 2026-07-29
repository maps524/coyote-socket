<!--
  The link state, in one word and one colour.

  Four states, not two, because "we have not been asked to connect" and
  "we are asking and being refused" call for different reactions, and a
  two-state pill would show the same grey dot for both.
-->
<script lang="ts">
  import type { LinkState } from './types'

  let { link, attempts = 0 }: { link: LinkState; attempts?: number } = $props()

  const labels: Record<LinkState, string> = {
    idle: 'Not connected',
    connecting: 'Connecting',
    connected: 'Connected',
    retrying: 'Retrying',
  }
</script>

<span class="pill {link}">
  <span class="dot"></span>
  {labels[link]}
  {#if link === 'retrying' && attempts > 1}
    <span class="count">attempt {attempts}</span>
  {/if}
</span>

<style>
  .pill {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    padding: 0.25rem 0.6rem;
    border-radius: 999px;
    border: 1px solid var(--line);
    background: var(--panel-2);
    font-size: 0.85rem;
    white-space: nowrap;
  }

  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--muted);
  }

  .connected .dot {
    background: var(--ok);
  }

  .connecting .dot {
    background: var(--info);
    animation: pulse 1s ease-in-out infinite;
  }

  .retrying .dot {
    background: var(--warn);
    animation: pulse 1s ease-in-out infinite;
  }

  .count {
    color: var(--muted);
    font-size: 0.78rem;
  }

  @keyframes pulse {
    50% {
      opacity: 0.3;
    }
  }

  /* A pulsing dot is decoration; nobody needs it enough to override this. */
  @media (prefers-reduced-motion: reduce) {
    .dot {
      animation: none;
    }
  }
</style>
