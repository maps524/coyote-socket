/**
 * The window's whole client-side model.
 *
 * One module of runes rather than a store per concern: everything here is
 * driven by three Tauri event streams and there is exactly one consumer, so
 * splitting it would add indirection without adding isolation.
 *
 * The backend owns the state; this only renders it. There is no polling loop
 * and no timer that asks Rust how things are going — the supervisor pushes a
 * snapshot on every change, and the UI redraws.
 */

import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import type { PlayerSnapshot, Reachability, Status, WireEvent } from './types'

/**
 * How many wire events to keep in the window.
 *
 * The Rust ring buffer keeps 1000 log lines and the copy button pulls from
 * there, so this bound only limits what is *rendered*. Past a few thousand
 * rows the table costs more than it tells you.
 */
const WIRE_LIMIT = 1500
const LOG_LIMIT = 800

function emptySnapshot(): PlayerSnapshot {
  return {
    type: 'player',
    link: 'idle',
    endpoint: '',
    epoch: 0,
    media: null,
    positionS: null,
    durationS: null,
    playing: null,
    speed: null,
    updatedAtMs: 0,
    packets: 0,
    fault: null,
    attempts: 0,
    stateSuspect: false,
  }
}

class Bridge {
  snapshot = $state<PlayerSnapshot>(emptySnapshot())
  status = $state<Status | null>(null)
  wire = $state<WireEvent[]>([])
  log = $state<string[]>([])

  /** Result of the most recent probe, shown next to the Connect button. */
  probe = $state<Reachability | null>(null)
  /** The last thing that went wrong in the UI itself, not on the wire. */
  error = $state<string | null>(null)
  busy = $state(false)

  /** Freeze the wire view so a burst can actually be read. */
  paused = $state(false)
  private pending: WireEvent[] = []

  qrSvg = $state<string | null>(null)

  /**
   * True once anything on the wire has failed to fit our reading of the
   * framing. Sticky on purpose: a framing failure is the most valuable result
   * this app can produce, and it must not scroll away.
   */
  framingFault = $derived(
    this.snapshot.fault?.kind === 'framing' ||
      this.wire.some((e) => e.kind === 'error'),
  )

  async init() {
    await this.refresh()

    await listen<PlayerSnapshot>('player-state', (event) => {
      this.snapshot = event.payload
    })

    await listen<WireEvent>('wire-event', (event) => {
      if (this.paused) {
        this.pending.push(event.payload)
        if (this.pending.length > WIRE_LIMIT) this.pending.shift()
        return
      }
      this.pushWire(event.payload)
    })

    await listen<string>('log-line', (event) => {
      this.log = [...this.log, event.payload].slice(-LOG_LIMIT)
    })

    this.log = await invoke<string[]>('log_history')
    this.qrSvg = await invoke<string>('pairing_qr').catch(() => null)
  }

  private pushWire(event: WireEvent) {
    const next = [...this.wire, event]
    this.wire = next.length > WIRE_LIMIT ? next.slice(-WIRE_LIMIT) : next
  }

  resume() {
    this.paused = false
    for (const event of this.pending) this.pushWire(event)
    this.pending = []
  }

  clearWire() {
    this.wire = []
    this.pending = []
  }

  async refresh() {
    this.status = await invoke<Status>('bridge_status')
    this.snapshot = this.status.snapshot
  }

  private async run<T>(action: () => Promise<T>): Promise<T | null> {
    this.busy = true
    this.error = null
    try {
      return await action()
    } catch (e) {
      this.error = String(e)
      return null
    } finally {
      this.busy = false
      await this.refresh()
    }
  }

  async connect(endpoint: string) {
    const result = await this.run(() =>
      invoke<Reachability>('connect', { endpoint }),
    )
    this.probe = result
  }

  async disconnect() {
    await this.run(() => invoke('disconnect'))
    this.probe = null
  }

  async check(endpoint: string) {
    const result = await this.run(() =>
      invoke<Reachability>('check_reachability', { endpoint }),
    )
    this.probe = result
  }

  async command(kind: 'play' | 'pause' | 'seek', positionS?: number) {
    await this.run(() => invoke('send_player_command', { kind, positionS }))
  }

  async startFakePlayer() {
    await this.run(() => invoke<string>('start_fake_player', { port: null }))
  }

  async stopFakePlayer() {
    await this.run(() => invoke('stop_fake_player'))
  }

  /**
   * Revoke the pairing token and take the new QR.
   *
   * Un-pairs every device at once — there is one shared token. The confirm
   * lives in the UI because that consequence is not guessable from the button.
   */
  async revokeToken() {
    const svg = await this.run(() => invoke<string>('rotate_token'))
    if (svg) this.qrSvg = svg
  }

  async open(url: string) {
    await this.run(() => invoke('open_external', { url }))
  }

  /** Everything a bug report needs, as one block of text. */
  async transcript(): Promise<string> {
    const lines = await invoke<string[]>('log_history')
    const header = [
      `coyote-bridge-app ${this.status?.version ?? '?'}`,
      `endpoint: ${this.snapshot.endpoint || '(none)'}`,
      `link: ${this.snapshot.link}`,
      this.snapshot.fault
        ? `fault: ${this.snapshot.fault.kind} — ${this.snapshot.fault.detail}`
        : 'fault: none',
      '',
      '--- wire ---',
    ]
    const wire = this.wire.map(formatWire)
    return [...header, ...wire, '', '--- log ---', ...lines].join('\n')
  }
}

/** One wire event as a line of text, prefix bytes included. */
export function formatWire(event: WireEvent): string {
  const time = new Date(event.atMs).toISOString().slice(11, 23)
  const arrow = event.dir === 'in' ? '<-' : '->'
  const prefix = event.prefixHex ? `[${event.prefixHex}] len=${event.len} ` : ''
  const note = event.note ? `  !! ${event.note}` : ''
  return `${time} ${event.source} ${arrow} ${prefix}${event.text ?? ''}${note}`
}

export const bridge = new Bridge()

/** Seconds as `m:ss`, or a dash when the player has not told us. */
export function clock(seconds: number | null): string {
  if (seconds === null || !Number.isFinite(seconds)) return '—'
  const total = Math.max(0, Math.floor(seconds))
  const minutes = Math.floor(total / 60)
  return `${minutes}:${String(total % 60).padStart(2, '0')}`
}
