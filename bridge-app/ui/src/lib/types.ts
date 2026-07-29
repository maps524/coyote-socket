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
  pairing: string
  local: string
  httpPort: number
}

export interface Status {
  snapshot: PlayerSnapshot
  urls: Urls
  httpError: string | null
  endpoint: string
  recents: string[]
  staticDir: string | null
  fakePlayer: string | null
  version: string
}
