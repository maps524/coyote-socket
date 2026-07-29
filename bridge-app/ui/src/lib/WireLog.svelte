<!--
  Every frame, raw, with the four length bytes that framed it.

  This is the deliverable. The spike report says so: the client has never
  spoken to a real player, so what a real player actually sends is the thing
  worth capturing, and a payload we failed to interpret is worth more than one
  we did. The prefix column is the reason this is a table rather than a text
  dump — `2c 01 00 00` next to `len=300` is self-checking, and if the two ever
  disagree with the payload that follows, the mismatch is visible without
  anyone having to know the protocol.
-->
<script lang="ts">
  import { bridge, formatWire } from './bridge.svelte'
  import type { WireEvent } from './types'

  let filter = $state<'all' | 'player' | 'fake'>('all')
  let hideHeartbeats = $state(true)
  let copied = $state(false)
  let viewport = $state<HTMLDivElement | null>(null)

  const rows = $derived(
    bridge.wire.filter(
      (event) =>
        (filter === 'all' || event.source === filter) &&
        !(hideHeartbeats && event.kind === 'heartbeat'),
    ),
  )

  // Follow the tail, but only while the view is already at the bottom —
  // yanking someone back down while they are reading an error is the fastest
  // way to make a log useless.
  $effect(() => {
    void rows.length
    const el = viewport
    if (!el || bridge.paused) return
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 80
    if (atBottom) queueMicrotask(() => el.scrollTo({ top: el.scrollHeight }))
  })

  async function copy() {
    await navigator.clipboard.writeText(await bridge.transcript())
    copied = true
    setTimeout(() => (copied = false), 1500)
  }

  function label(event: WireEvent): string {
    if (event.kind === 'event') return ''
    return `${event.dir === 'in' ? '←' : '→'}`
  }
</script>

<section class="panel wire">
  <div class="head">
    <h2>Wire log</h2>
    <div class="row">
      <select bind:value={filter} aria-label="Filter by source">
        <option value="all">Both</option>
        <option value="player">Player link</option>
        <option value="fake">Test player</option>
      </select>
      <label class="small">
        <input type="checkbox" bind:checked={hideHeartbeats} />
        Hide heartbeats
      </label>
      <button onclick={() => (bridge.paused ? bridge.resume() : (bridge.paused = true))}>
        {bridge.paused ? 'Resume' : 'Pause'}
      </button>
      <button onclick={() => bridge.clearWire()}>Clear</button>
      <button class="primary" onclick={copy}>{copied ? 'Copied' : 'Copy all'}</button>
    </div>
  </div>

  <p class="small muted intro">
    Every frame, before we interpret it. <strong>Copy all</strong> includes the
    log as well — that is what to paste back if anything here looks wrong.
  </p>

  <div class="viewport mono" bind:this={viewport}>
    {#if rows.length === 0}
      <p class="empty muted">Nothing on the wire yet.</p>
    {:else}
      {#each rows as event (event.seq)}
        <div class="line {event.kind}" title={formatWire(event)}>
          <span class="time">{new Date(event.atMs).toISOString().slice(11, 23)}</span>
          <span class="src {event.source}">{event.source === 'fake' ? 'fake' : 'plr'}</span>
          <span class="dir">{label(event)}</span>
          <span class="prefix">{event.prefixHex}</span>
          <span class="len">{event.prefixHex ? event.len : ''}</span>
          <span class="text">
            {event.kind === 'heartbeat' ? '(heartbeat)' : (event.text ?? '')}
          </span>
        </div>
        {#if event.note}
          <div class="note">{event.note}</div>
        {/if}
      {/each}
    {/if}
  </div>
</section>

<style>
  .wire {
    display: flex;
    flex-direction: column;
    min-height: 0;
  }

  .head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 0.5rem;
    flex-wrap: wrap;
  }

  .head h2 {
    margin: 0;
  }

  .intro {
    margin: 0.5rem 0 0.6rem;
  }

  .viewport {
    flex: 1;
    min-height: 12rem;
    overflow: auto;
    background: #08080c;
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 0.5rem;
    font-size: 0.78rem;
    line-height: 1.5;
  }

  .empty {
    margin: 0;
  }

  .line {
    display: grid;
    grid-template-columns: 6.5rem 2.2rem 1.2rem 8.5rem 4rem 1fr;
    gap: 0.4rem;
    white-space: nowrap;
  }

  .line .text {
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .time,
  .len {
    color: var(--muted);
  }

  .prefix {
    color: var(--info);
  }

  .src {
    color: var(--muted);
  }

  .src.fake {
    color: var(--accent);
  }

  .line.heartbeat {
    opacity: 0.5;
  }

  .line.event .text {
    color: var(--info);
  }

  .line.error .text {
    color: var(--bad);
  }

  /* A diagnosis wraps and sits under its frame, because it is a sentence and
     the frames are columns. */
  .note {
    color: var(--bad);
    white-space: normal;
    padding: 0.2rem 0 0.4rem 6.9rem;
    max-width: 70ch;
  }
</style>
