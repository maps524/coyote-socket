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
  httpError: string | null
  endpoint: string
  recents: string[]
  staticDir: string | null
  fakePlayer: string | null
  version: string
}
