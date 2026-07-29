<!--
  Who is connected to the bridge.

  This panel exists because "is my phone actually talking to this?" was
  unanswerable from the desktop, and an evening of debugging was spent guessing
  at it. So the first duty of every number here is to be one you can act on.

  That makes the interesting case the one where we do not know. A phone that
  roams between access points drops its socket and opens a new one; unless the
  client says which browser it is, the bridge cannot tell that from a second
  phone arriving. Where that is so, this panel says so — a range and the reason
  for it, never a confident count. `clients.rs` sets out exactly which signals
  are and are not identity, and why none of the ones the bridge can observe on
  its own qualifies.
-->
<script lang="ts">
  import { bridge } from './bridge.svelte'
  import type { ClientView } from './types'

  const view = $derived(bridge.clients)

  /**
   * A display clock, so "connected 4s ago" ages without a round trip.
   *
   * Not polling: nothing is asked of the backend. The backend pushes the set
   * of clients when it changes, and this only re-renders the ages of values
   * already delivered.
   */
  let now = $state(Date.now())
  $effect(() => {
    const timer = setInterval(() => (now = Date.now()), 1000)
    return () => clearInterval(timer)
  })

  /**
   * The browser range.
   *
   * Floor: every presented id is certainly a browser, and one unattributable
   * socket is certainly at least one. Ceiling: every unattributable socket
   * could be its own. When the two meet, the range is a fact — one
   * unidentified socket really is exactly one browser.
   */
  const range = $derived.by(() => {
    const b = view.browsers
    if (b.state === 'reported') return { lo: b.count, hi: b.count }
    return {
      lo: Math.max(b.count, b.unidentified > 0 ? 1 : 0),
      hi: b.count + b.unidentified,
    }
  })

  const unidentifiedCount = $derived(
    view.browsers.state === 'atLeast' ? view.browsers.unidentified : 0,
  )

  /**
   * Whether the count rests on ids the clients chose for themselves.
   *
   * Keyed off the count's own provenance rather than off whether the bridge
   * *can* verify devices. Those come apart the moment pairing exists: with one
   * paired phone and one unpaired browser, `credentialsAvailable` is true while
   * the number in front of the user is still only as good as what the unpaired
   * one volunteered. Wording that flipped to "verified" on the first paired
   * device would be confidently wrong for every mixed view thereafter.
   */
  const countIsVolunteered = $derived(
    view.browsers.state === 'reported' &&
      view.browsers.count > 0 &&
      view.browsers.provenance === 'selfReported',
  )
  const countIsVerified = $derived(
    view.browsers.state === 'reported' &&
      view.browsers.count > 0 &&
      view.browsers.provenance === 'credential',
  )

  /** How long since `ms`, in the coarsest unit that is still useful. */
  function ago(ms: number): string {
    const seconds = Math.max(0, Math.round((now - ms) / 1000))
    if (seconds < 60) return `${seconds}s`
    if (seconds < 3600) return `${Math.floor(seconds / 60)}m`
    return `${Math.floor(seconds / 3600)}h`
  }

  function clockTime(ms: number): string {
    return new Date(ms).toLocaleTimeString()
  }

  /**
   * The row's headline.
   *
   * A label is only promoted to the headline when it came with a verified
   * credential — that one was typed by the user, into their own bridge. An
   * unverified client also gets to send a `label`, and rendering that as the
   * row's name would hand the most prominent text on the panel to whoever
   * holds the pairing token: "Sara's iPhone", or "verified", chosen by the
   * connection you are trying to identify. Those are shown, but as something
   * the client *said* — see `claimedName`.
   */
  function name(client: ClientView): string {
    if (client.provenance === 'credential') {
      return client.label ?? 'Unnamed browser'
    }
    return client.identified ? 'Self-named client' : 'Unidentified client'
  }

  /** A label the client chose, when it is not entitled to the headline. */
  function claimedName(client: ClientView): string | null {
    return client.provenance === 'credential' ? null : client.label
  }

  /**
   * Whether a connection looks wedged.
   *
   * The relay writes to every client at least once a second, so a client we
   * have not managed to write to for several seconds is probably already gone
   * — a phone carried out of range holds a half-open socket for a while. A
   * bare "connected" would keep vouching for it.
   */
  const STALE_AFTER_MS = 5000
  function stale(client: ClientView): boolean {
    return client.connected && now - client.lastHeardMs > STALE_AFTER_MS
  }

  function plural(n: number, word: string): string {
    return `${n} ${word}${n === 1 ? '' : 's'}`
  }
</script>

<section class="panel">
  <h2>Clients</h2>

  {#if view.connections === 0}
    <p class="small muted">
      Nothing is connected. A phone with the paired page open appears here
      within a second of opening it.
    </p>
  {:else}
    <p class="summary">
      <!-- Sockets are counted, not inferred. This is the one number here that
           needs no qualification, so it leads. -->
      <strong>{plural(view.connections, 'connection')}</strong>
      <span class="muted">·</span>
      {#if range.lo === range.hi}
        <!--
          Still hedged when the ids behind it were merely volunteered. A flat
          "2 browsers" reads as a measurement, and a count assembled from
          strings anyone with the pairing token could have sent is not one.
        -->
        <span class:uncertain={countIsVolunteered}>
          {plural(range.lo, 'browser')}
        </span>
      {:else}
        <span class="uncertain">{range.lo}–{range.hi} browsers</span>
      {/if}
    </p>

    {#if countIsVolunteered}
      <p class="small muted note">
        Counted by the ids the clients sent for themselves. Anyone holding the
        pairing token can send any id, so two of these could be one browser, or
        one could be two.
      </p>
    {/if}

    {#if unidentifiedCount > 0}
      <!--
        The honest state, and today's default: no client sends an id yet. Say
        what is missing and what follows from it, rather than picking a number
        and hoping. A count that silently merges a roam — or splits one — is
        worse than no count, because it is the number someone acts on.
      -->
      <p class="small muted note">
        {unidentifiedCount === 1
          ? 'One connection did not identify itself'
          : `${unidentifiedCount} connections did not identify themselves`}, so
        a phone that reconnected and a second phone look the same here.
      </p>
    {/if}
  {/if}

  <ul class="clients">
    {#each view.clients as client (client.key)}
      <li class:gone={!client.connected}>
        <div class="head">
          <span
            class="dot"
            class:live={client.connected && !stale(client)}
            class:warn={client.connected && stale(client)}
            aria-hidden="true"
          ></span>
          <span class="name">{name(client)}</span>
          <!--
            The two grades of identity read differently on purpose. A verified
            credential survives someone trying to forge it; a self-reported id
            does not, and a panel that rendered them alike would be trusted
            further than it earns.
          -->
          {#if client.revokeContested}
            <span
              class="tag bad"
              title="This device was revoked and has connected again. Its credential was never deleted."
            >
              revoke failed
            </span>
          {:else if client.revokedAtMs !== null}
            <!--
              Takes precedence over "verified", which is what this row would
              otherwise still be claiming for a credential that has just been
              deleted — on the one screen someone consults to check that the
              revoke worked.
            -->
            <span class="tag bad" title="This device's credential was deleted. It cannot reconnect.">
              revoked
            </span>
          {:else if client.provenance === 'credential'}
            <span class="tag ok" title="A per-device credential this bridge verified.">
              verified
            </span>
          {:else if client.provenance === 'selfReported'}
            <span
              class="tag"
              title="An id the client chose for itself. Anyone with the pairing token could send any id."
            >
              self-reported
            </span>
          {:else}
            <span class="tag" title="This client did not say which browser it is.">
              unidentified
            </span>
          {/if}
          {#if client.sockets > 1}
            <span class="tag">{client.sockets} sockets</span>
          {/if}
        </div>

        <div class="small muted facts">
          {#if claimedName(client)}
            <!--
              Attributed, and in quotes. This text was chosen by the connection
              itself, so presenting it plainly would let whoever holds the
              pairing token write the most prominent words on the panel.
            -->
            <span>calls itself &ldquo;{claimedName(client)}&rdquo;</span>
          {/if}
          <span class="mono">{client.address}</span>
          {#if client.agent}<span>{client.agent}</span>{/if}
          {#if client.connected}
            <span>connected {clockTime(client.connectedAtMs)} · {ago(client.connectedAtMs)} ago</span>
          {:else if client.disconnectedAtMs !== null}
            <span>left {ago(client.disconnectedAtMs)} ago</span>
          {/if}
          {#if client.revokedAtMs !== null && !client.revokeContested}
            <span>revoked {ago(client.revokedAtMs)} ago — cannot reconnect</span>
          {/if}
          {#if client.createdMs !== null}
            <span>paired {new Date(client.createdMs).toLocaleDateString()}</span>
          {/if}
          {#if client.connections > 1}
            <!-- Stated rather than left for the reader to work out from two
                 rows that happen to share an address. -->
            <span>connected {client.connections} times this session</span>
          {/if}
        </div>

        {#if client.previousAddresses.length > 0}
          <p class="small muted facts">
            <span>moved from</span>
            <span class="mono">{client.previousAddresses.join(', ')}</span>
          </p>
        {/if}

        {#if client.identified && client.id}
          <p
            class="small muted id mono"
            title={client.provenance === 'credential'
              ? 'The non-secret half of this device’s credential. Safe to paste into a bug report.'
              : 'An id the client chose for itself. Not a credential and not checked.'}
          >
            {client.id}
          </p>
        {/if}

        {#if client.impersonating}
          <!--
            Loud, because the panel is what someone reads before revoking, and
            two rows showing the same id with nothing between them is how the
            wrong one gets picked. The counts are already correct — this is
            about the reader, not the arithmetic.
          -->
          <p class="small bad">
            <strong>This client is using a verified device's id.</strong>
            It did not present that device's credential, so it is a different
            connection claiming the same name. The verified row is the other
            one; this one cannot be revoked because there is nothing durable
            behind it.
          </p>
        {/if}

        {#if client.revokeContested}
          <!--
            The loudest thing this panel can say, and it earns it. The socket
            was closed and the credential was not deleted, so the device came
            straight back — and every other reading of this row would be
            reassuring about a phone that is connected right now.
          -->
          <p class="small bad">
            <strong>This device was revoked and has come back.</strong>
            Its credential still works, so it was never deleted — only its
            connection was closed. It is connected now. Revoke it again, and if
            it returns a second time the credential store is not saving the
            change.
          </p>
        {/if}

        {#if stale(client)}
          <p class="small warn">
            This client has not answered for {ago(client.lastHeardMs)}. The
            socket is still open, which by itself proves nothing — a phone
            carried out of range holds one for a long time.
          </p>
        {/if}

        {#if client.maybeSameAs}
          <!--
            A guess, and it is labelled as one. It never moves the counts above;
            merging on "same address, same browser" is exactly the confidently
            wrong number this panel exists to avoid.
          -->
          <p class="small muted">
            Possibly the client that dropped moments ago, from the same address
            and the same browser — <em>not confirmed</em>, and not counted as
            the same browser.
          </p>
        {/if}
      </li>
    {/each}
  </ul>

  <!--
    Kept visible even when everything is identified, because "reported" is not
    "verified" and the difference matters the moment someone trusts the number.
  -->
  <p class="small muted footnote">
    <!--
      Three cases, because two would have to lump the mixed one in with either
      the verified or the unverified world, and it belongs to neither. The
      condition is the *count's* provenance, not whether the bridge is capable
      of verifying — those diverge the moment one device has paired and another
      has not.
    -->
    {#if countIsVerified}
      <!--
        The identity is good now, and the caveats are still real. A credential
        answers "is this the same browser as last time" and nothing else, so
        the panel says browsers and leaves the headcount to the labels.
      -->
      Every row here was verified by this bridge. A verified row is a
      <em>browser</em>, not a handset and not a person: Safari and a
      home-screen install on the same phone can hold separate credentials and
      show as two, and a browser whose storage was cleared is a new one with no
      way to know it was the old one.
    {:else if view.credentialsAvailable}
      Identity here is whatever each client volunteered, and an unpaired client
      can volunteer anything. Pairing a device replaces its guess with something
      this bridge checked — until then an address identifies nothing, since NAT,
      DHCP and a roaming phone all break it.
    {:else}
      This bridge cannot verify anything: no device has paired. Identity here
      is whatever a client volunteers, and the bridge cannot work it out on its
      own — an address identifies nothing, since NAT, DHCP and a roaming phone
      all break it, and the pairing token is shared by every device by design.
    {/if}
  </p>
</section>

<style>
  .summary {
    margin: 0 0 0.3rem;
    display: flex;
    gap: 0.4rem;
    align-items: baseline;
    flex-wrap: wrap;
  }

  /* A range is not a defect, but it should not read like a settled number. */
  .uncertain {
    color: var(--warn);
  }

  .note {
    margin: 0 0 0.4rem;
  }

  .clients {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }

  .clients li {
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 0.45rem 0.6rem;
    background: var(--panel-2);
  }

  .clients li.gone {
    opacity: 0.6;
    border-style: dashed;
  }

  .head {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    flex-wrap: wrap;
  }

  .name {
    font-weight: 600;
  }

  .dot {
    width: 0.5rem;
    height: 0.5rem;
    border-radius: 50%;
    background: var(--muted);
    flex: none;
  }

  .dot.live {
    background: var(--ok);
  }

  .dot.warn {
    background: var(--warn);
  }

  .tag.bad {
    color: var(--bad);
    border-color: color-mix(in srgb, var(--bad) 40%, var(--line));
  }

  .tag.ok {
    color: var(--ok);
    border-color: color-mix(in srgb, var(--ok) 40%, var(--line));
  }

  .tag {
    font-size: 0.72rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--muted);
    border: 1px solid var(--line);
    border-radius: 999px;
    padding: 0.05rem 0.4rem;
  }

  .facts {
    display: flex;
    gap: 0.6rem;
    flex-wrap: wrap;
    margin: 0.2rem 0 0;
  }

  .id {
    margin: 0.2rem 0 0;
    overflow-wrap: anywhere;
    opacity: 0.7;
  }

  .warn {
    color: var(--warn);
  }

  .bad {
    color: var(--bad);
  }

  .footnote {
    margin: 0.7rem 0 0;
    padding-top: 0.6rem;
    border-top: 1px solid var(--line);
  }
</style>
