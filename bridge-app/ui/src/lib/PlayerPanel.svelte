<!--
  Live playback, as the player reports it.

  This is the panel that answers the question the spike was built to ask: does
  a real DeoVR or HereSphere actually stream position over 23554 in the shape
  we think it does? So position is the largest thing here, and the packet
  counter sits next to it — "connected but silent" and "connected and
  streaming" look identical without one.

  Unknown is rendered as "—", never as zero. A consumer that treated a missing
  position as zero would jump a script to the start, and the same mistake in
  the UI would hide it.
-->
<script lang="ts">
  import { bridge, clock } from './bridge.svelte'

  const snap = $derived(bridge.snapshot)
  const connected = $derived(snap.link === 'connected')
  const progress = $derived(
    snap.positionS !== null && snap.durationS
      ? Math.min(100, (snap.positionS / snap.durationS) * 100)
      : 0,
  )

  // Age of the last packet, refreshed by a ticking clock rather than by a
  // backend poll. The backend pushes state; this only measures how long ago
  // it last did.
  let now = $state(Date.now())
  $effect(() => {
    const timer = setInterval(() => (now = Date.now()), 500)
    return () => clearInterval(timer)
  })
  const ageMs = $derived(snap.updatedAtMs ? now - snap.updatedAtMs : null)
  const stale = $derived(connected && ageMs !== null && ageMs > 3000)

  // While a drag is in progress the bar follows the finger, not the player.
  // Otherwise the next 1 Hz packet snaps it back to where playback still is
  // and the control fights the user.
  let scrubbing = $state(false)
  let scrubTo = $state(0)
</script>

<section class="panel">
  <h2>Playback</h2>

  {#if !connected}
    <p class="muted idle">
      Nothing is streaming. Connect to a player to see position here.
    </p>
  {:else}
    <div class="clock mono">
      <span class="position">{clock(snap.positionS)}</span>
      <span class="muted">/ {clock(snap.durationS)}</span>
      <span class="state" class:playing={snap.playing} class:suspect={snap.stateSuspect}>
        {snap.playing === null ? 'unknown' : snap.playing ? 'playing' : 'paused'}
      </span>
    </div>

    <!--
      Draggable, because "seek works" was proven end to end against a real
      Quest and a bar you cannot drag is a worse version of a feature that
      already exists. A native range input rather than a custom drag handler:
      it gets keyboard control, touch, and accessible semantics for free.

      Seek fires on release, not on every input event. The player is on the
      other end of a TCP link with a 1 Hz heartbeat; flooding it with a
      command per pixel is how you find out what its input queue does.
    -->
    <input
      class="scrub"
      type="range"
      min="0"
      max={snap.durationS ?? 0}
      step="0.1"
      value={scrubbing ? scrubTo : (snap.positionS ?? 0)}
      disabled={!snap.durationS}
      aria-label="Seek"
      oninput={(e) => {
        scrubbing = true
        scrubTo = Number(e.currentTarget.value)
      }}
      onchange={(e) => {
        scrubbing = false
        bridge.command('seek', Number(e.currentTarget.value))
      }}
      style="--progress: {scrubbing && snap.durationS
        ? (scrubTo / snap.durationS) * 100
        : progress}%"
    />

    <dl>
      <dt>File</dt>
      <dd class="mono break" title={snap.media ?? ''}>{snap.media ?? '—'}</dd>
      <dt>Speed</dt>
      <dd>{snap.speed !== null ? `${snap.speed}×` : '—'}</dd>
      <dt>Packets</dt>
      <dd>{snap.packets}</dd>
      <dt>Last packet</dt>
      <dd class:stale>
        {ageMs === null ? '—' : `${(ageMs / 1000).toFixed(1)}s ago`}
      </dd>
    </dl>

    {#if snap.stateSuspect}
      <p class="small warn">
        The player reports itself paused while its position advances in step
        with the clock. DeoVR's <code>playerState</code> echoes the last value a
        remote client set rather than what the player is doing, so treat it as
        advisory — the position is the reliable signal.
      </p>
    {/if}

    {#if stale}
      <p class="small warn">
        Connected, but nothing has arrived for {(ageMs! / 1000).toFixed(0)}s.
        Either playback is stopped, or the player is not sending what we expect.
      </p>
    {/if}

    <div class="row controls">
      <button onclick={() => bridge.command('play')}>Play</button>
      <button onclick={() => bridge.command('pause')}>Pause</button>
      <button
        onclick={() => bridge.command('seek', Math.max(0, (snap.positionS ?? 0) - 10))}
      >
        −10s
      </button>
      <button
        onclick={() => bridge.command('seek', (snap.positionS ?? 0) + 10)}
      >
        +10s
      </button>
      <span class="small muted">
        If these move the video, the player parsed something we wrote.
      </span>
    </div>
  {/if}
</section>

<style>
  .idle {
    margin: 0;
  }

  .clock {
    display: flex;
    align-items: baseline;
    gap: 0.6rem;
  }

  .position {
    font-size: 2rem;
    font-variant-numeric: tabular-nums;
  }

  .state {
    margin-left: auto;
    font-size: 0.85rem;
    color: var(--muted);
  }

  .state.playing {
    color: var(--ok);
  }

  .scrub {
    display: block;
    width: 100%;
    margin: 0.6rem 0 0.9rem;
    appearance: none;
    background: linear-gradient(
      to right,
      var(--accent) var(--progress),
      var(--panel-2) var(--progress)
    );
    height: 6px;
    border-radius: 3px;
    border: 0;
    padding: 0;
    cursor: pointer;
  }

  .scrub:disabled {
    cursor: default;
    opacity: 0.5;
  }

  .scrub::-webkit-slider-thumb {
    appearance: none;
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background: var(--accent);
    border: 2px solid var(--bg);
  }

  .scrub::-moz-range-thumb {
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background: var(--accent);
    border: 2px solid var(--bg);
  }

  .state.suspect {
    color: var(--warn);
    text-decoration: underline dotted;
  }

  dl {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 0.3rem 0.9rem;
    margin: 0;
    font-size: 0.88rem;
  }

  dt {
    color: var(--muted);
  }

  dd {
    margin: 0;
    min-width: 0;
  }

  .break {
    overflow-wrap: anywhere;
  }

  .stale {
    color: var(--warn);
  }

  .warn {
    color: var(--warn);
  }

  .controls {
    margin-top: 0.9rem;
  }
</style>
