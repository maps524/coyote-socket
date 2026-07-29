/**
 * Mirrors of the Rust types that cross the IPC boundary.
 *
 * Hand-written rather than generated: there are six of them and a generator
 * would be more machinery than this spike is worth. The Rust side is the
 * authority — `state.rs`, `probe.rs` and `wire.rs` — and any disagreement here
 * shows up immediately as an undefined field in the window.
 */

export type LinkState = 'idle' | 'connecting' | 'connected' | 'retrying'

export type FaultKind =
  | 'refused'
  | 'timed-out'
  | 'unreachable'
  | 'address'
  | 'closed'
  | 'framing'

export interface LinkFault {
  kind: FaultKind
  /** The underlying error, verbatim. Never paraphrased away. */
  detail: string
  hint: string | null
}

export interface PlayerSnapshot {
  type: 'player'
  link: LinkState
  endpoint: string
  /**
   * Bumped on every successful connection.
   *
   * A change means the position before it and the position after it are
   * unrelated — the recorded capture jumped backwards 131.7 s across a
   * reconnect. State arrives over a coalescing channel, so the intermediate
   * "disconnected" states may never be delivered; this survives that.
   */
  epoch: number
  media: string | null
  /** Null means "the player has not told us", which is not position zero. */
  positionS: number | null
  durationS: number | null
  playing: boolean | null
  speed: number | null
  updatedAtMs: number
  packets: number
  fault: LinkFault | null
  attempts: number
  /**
   * The player calls itself paused while its position advances.
   *
   * Observed against a real DeoVR: `playerState` echoes the last value a
   * remote client set rather than reporting what the player is doing.
   */
  stateSuspect: boolean
}

export type ReachabilityOutcome =
  | 'port-open'
  | 'port-closed'
  | 'host-up-port-filtered'
  | 'no-response'
  | 'bad-address'

export interface Reachability {
  outcome: ReachabilityOutcome
  detail?: string
}

export type WireSource = 'player' | 'fake'
export type WireDir = 'in' | 'out'
export type WireKind = 'heartbeat' | 'json' | 'error' | 'event'

export interface WireEvent {
  seq: number
  atMs: number
  source: WireSource
  dir: WireDir
  kind: WireKind
  /** The four length bytes exactly as they came off the wire. */
  prefixHex: string
  /** What we decoded that prefix as. */
  len: number
  text: string | null
  /** A plain-language diagnosis when the numbers look wrong. */
  note: string | null
}

export interface Urls {
  /** Without the token. The full URL is `Status.pairingUrl`. */
  pairingBase: string
  local: string
  httpPort: number
}

/**
 * Whether the phone-facing server is actually listening.
 *
 * Three-valued on purpose. It used to be an optional error string, where
 * "no error" also meant "not asked yet" — so the window rendered a QR and
 * reported no problem before anything had bound to the port.
 */
export type HttpStatus =
  | { state: 'starting' }
  | { state: 'serving' }
  | { state: 'failed'; detail: string }

/**
 * Whether the phone can get a secure context — what Web Bluetooth requires.
 *
 * Same shape as `HttpStatus` and for the same reason: it is set from the TLS
 * bind, not from "a certificate was issued". Those two differ exactly when
 * another bridge already holds the port, which is the ordinary case while
 * developing one.
 *
 * `notConfigured` is separate from `failed` because they need different
 * sentences — one is a choice, the other is a fault.
 */
export type TlsStatus =
  | { state: 'starting' }
  | { state: 'serving' }
  | { state: 'notConfigured'; detail: string }
  | { state: 'failed'; detail: string }

/**
 * Where a row's identity came from.
 *
 * `credential` is a per-device credential the bridge verified — the thing that
 * actually tells a roam from a second device. `selfReported` is a string the
 * client chose, worth exactly what it costs to forge. The panel shows which,
 * because treating them as equal is how an identity panel gets trusted further
 * than it deserves.
 */
export type Provenance = 'selfReported' | 'credential'

/**
 * How many distinct browsers are connected — or why the bridge will not say.
 *
 * Browsers, not devices: a credential lives in one browser's storage
 * partition, so Safari and a home-screen install on one phone can be two, and
 * a cleared browser is a new one.
 *
 * Two-valued for the same reason `HttpStatus` is three-valued: a bare number
 * would be a field promising knowledge nobody confirmed. `reported` is still
 * not certainty — it is a count of ids presented, which is why it is not
 * called `known`.
 */
export type BrowserCount =
  | { state: 'reported'; count: number; provenance: Provenance }
  | { state: 'atLeast'; count: number; unidentified: number }

/** One identity group: a client, or a connection we cannot attribute. */
export interface ClientView {
  /** Stable while the row exists. For keying the list, not for identity. */
  key: string
  identified: boolean
  /** Where that identity came from. Null when there is none. */
  provenance: Provenance | null
  /** The identity. For a credential this is the non-secret half. */
  id: string | null
  /** User-set on a credential; client-chosen on a self-reported row. */
  label: string | null
  /** When this device first paired, when the credential store records it. */
  createdMs: number | null
  /**
   * This row volunteers an id that a verified credential also holds.
   *
   * The two never merge, so the counts are right without this. But both render
   * the same id string, and the panel is what someone reads before choosing
   * which row to revoke.
   */
  impersonating: boolean
  /**
   * Whether "revoke this device" is coherent here. Only a verified credential
   * that has not already been revoked: revoking a self-reported id closes a
   * socket that reconnects a second later under any id it likes.
   */
  revocable: boolean
  /**
   * When this device was revoked, if it was, during this run.
   *
   * The row outlives its sockets by minutes — exactly the minutes someone
   * spends checking the revoke worked — so it says what happened rather than
   * sitting there still tagged "verified".
   */
  revokedAtMs: number | null
  /**
   * A revoked device reconnected with a credential that still verifies.
   *
   * A detector, not a state: it can only happen when the credential was never
   * deleted, so the revoke closed a socket and nothing more. Rendered as a
   * fault, because the alternative is a row reading "revoked" with a live
   * phone attached to it.
   */
  revokeContested: boolean
  connected: boolean
  /** Open sockets. Two tabs on one phone is two sockets, one device. */
  sockets: number
  /** Connections during this bridge run. Above one means it reconnected. */
  connections: number
  firstSeenMs: number
  connectedAtMs: number
  disconnectedAtMs: number | null
  /**
   * When a frame last arrived **from** this client.
   *
   * Read that literally. It used to be set after a successful *write*, which
   * proves only that the local TCP stack took the bytes — so the field named
   * "last heard" could not detect the one case it existed for. The relay now
   * pings each keepalive and browsers answer automatically, so this is the
   * client's own signal.
   *
   * Stale implies dead. Fresh implies the browser is reachable, which is not
   * quite the page being healthy: a Pong is answered by the browser, not by
   * the app's script.
   */
  lastHeardMs: number
  address: string
  /** Earlier addresses. Non-empty on an identified row means it roamed. */
  previousAddresses: string[]
  agent: string | null
  /**
   * A guess, rendered as one: a gone row from the same address and agent that
   * dropped moments ago. Never folded into `devices`.
   */
  maybeSameAs: string | null
}

export interface ClientsView {
  /** Open sockets. The one number here that is measured, not inferred. */
  connections: number
  browsers: BrowserCount
  clients: ClientView[]
  anyUnidentified: boolean
  /**
   * Whether this bridge can verify a device at all.
   *
   * True once a credential resolver is installed — which the app and the
   * headless binary both do at startup, now that pairing exists. False
   * therefore no longer means "this feature was never built"; it means the
   * wiring is missing on this build, and the log says so on the first
   * credentialed connection.
   *
   * Stated rather than left to be inferred from an absence: without it, a
   * bridge whose resolver failed to install is indistinguishable from one
   * where nobody has paired.
   */
  credentialsAvailable: boolean
}

export interface Status {
  snapshot: PlayerSnapshot
  urls: Urls
  /**
   * The full pairing URL with the live token — password-equivalent.
   *
   * Derived per call rather than stored, so revoking a token changes what the
   * window shows instead of leaving it advertising a dead one.
   */
  pairingUrl: string
  http: HttpStatus
  tls: TlsStatus
  endpoint: string
  recents: string[]
  staticDir: string | null
  libraryDir: string | null
  fakePlayer: string | null
  version: string
  /** Who is connected. Also pushed on change as the `clients` event. */
  clients: ClientsView
}
