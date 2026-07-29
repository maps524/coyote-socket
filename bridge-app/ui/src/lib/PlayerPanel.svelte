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
  //
  // `scrubbing` is cleared on pointer release *and* on cancel and blur, not
  // only on `change`. A drag that ends without a change event — a touch
  // cancelled by a scroll, a drag abandoned outside the window — would
  // otherwise leave the bar frozen at `scrubTo` while playback advanced
  // underneath it, showing a confidently wrong position indefinitely.
  let scrubbing = $state(false)
  let scrubTo = $state(0)
  // Which file the drag started on, so a media change mid-drag cannot commit
  // a position measured against a timeline that no longer exists.
  let dragMedia = $state<string | null>(null)

  // A reconnect is a discontinuity: the epoch changing means the position
  // before and after are unrelated. Abandon any drag in flight rather than
  // letting its release seek the new connection.
  //
  // Compared against a captured value rather than left to effect granularity.
  // `snap` is a fresh object on every packet, and `$effect` tracks the signal
  // rather than the property path — so `void snap.epoch` re-ran about once a
  // second and cleared `scrubbing` mid-drag. `oninput` put it back on every
  // pointer move, which hid the bug while the finger was moving and exposed it
  // the moment someone held still: the thumb snapped to the playhead under
  // their finger. That is precisely the behaviour `scrubbing` exists to
  // prevent, reintroduced by the fix for something else.
  let lastEpoch = $state(0)
  $effect(() => {
    if (snap.epoch !== lastEpoch) {
      lastEpoch = snap.epoch
      scrubbing = false
      dragMedia = null
    }
  })
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
      max={snap.durationS ?? 1}
      step="0.1"
      value={scrubbing ? scrubTo : (snap.positionS ?? 0)}
      disabled={!snap.durationS || snap.positionS === null}
      aria-label="Seek"
      oninput={(e) => {
        scrubbing = true
        scrubTo = Number(e.currentTarget.value)
      }}
      onchange={(e) => {
        const target = Number(e.currentTarget.value)
        scrubbing = false
        // Only commit if the media is still the one that was being dragged.
        // A media change mid-drag replaces duration, and releasing would
        // otherwise seek the *new* file to a position measured against the
        // old one's timeline.
        if (dragMedia === snap.media) bridge.command('seek', target)
      }}
      onpointerdown={() => (dragMedia = snap.media)}
      onpointerup={() => (scrubbing = false)}
      onpointercancel={() => (scrubbing = false)}
      onblur={() => (scrubbing = false)}
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
      <!--
        Disabled while position is unknown. `positionS ?? 0` would make "+10s"
        mean "seek to 10.0" on a player that has not told us where it is —
        which is the missing-means-zero mistake this file's own header warns
        about, and it would move the video rather than fail visibly.
      -->
      <button
        disabled={snap.positionS === null}
        onclick={() => bridge.command('seek', Math.max(0, snap.positionS! - 10))}
      >
        −10s
      </button>
      <button
        disabled={snap.positionS === null}
        onclick={() => bridge.command('seek', snap.positionS! + 10)}
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
