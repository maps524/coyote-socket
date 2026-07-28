# Plugin System

## What it needs to support

Four extension points, all of which map cleanly onto interfaces already implied by the architecture:

| Extension point | What a plugin provides | Example |
|---|---|---|
| **SyncSource** | where playback currently is | a new video player, a live stream, a metronome |
| **InputSource** | axis values per tick | funscript variants, audio-reactive, biometric, a MIDI pad |
| **ScriptRepository** | a searchable library | Stash, XBVR, a private index, EroScripts |
| **OutputTarget** | somewhere to send resolved values | buttplug.io, The Handy, an ESP32 over WebSocket |

MFP does this with a **C# plugin system** — plugins are compiled at runtime by `PluginCompiler.cs`
against a `PluginBase`. That's the model to imitate, in a browser-appropriate way.

## The browser version

Plugins are **ES modules loaded at runtime from a URL**, executed inside a **Web Worker**, talking to
the host over a narrow `postMessage` API.

```
┌──────────────┐   fetch + SRI check   ┌────────────────┐
│ plugin index │ ─────────────────────▶│ blob: URL      │
│  (JSON)      │                       └───────┬────────┘
└──────────────┘                               │ new Worker(blob)
                                               ▼
                                    ┌────────────────────────┐
                                    │ Plugin Worker           │
                                    │  — no DOM               │
                                    │  — fetch/WebSocket only │
                                    │  — no Bluetooth access  │
                                    └───────────┬────────────┘
                                                │ postMessage
                                    ┌───────────▼────────────┐
                                    │ Host: routes to        │
                                    │ SyncSource / Input /   │
                                    │ Repository registries  │
                                    └────────────────────────┘
```

### Why a Worker, not `import()` on the main thread

1. **Safety.** A plugin must never be able to write to the BLE characteristic directly. Running in a
   Worker means it *structurally cannot* — Web Bluetooth isn't exposed to workers, and the host owns
   the write path and the clamps.
2. **CSP.** Allowing arbitrary remote `script-src` on the main document destroys your CSP. Fetching
   the module text, verifying it, and instantiating it as a `blob:` worker keeps the document's CSP
   tight and confines `worker-src blob:` to the sandbox.
3. **Jank.** A badly written plugin polling something every 10 ms won't stall the tick loop or the UI.

### Manifest

A plugin registry is just a static JSON file — anyone can host one, users add registry URLs in
settings:

```json
{
  "name": "Community Plugins",
  "plugins": [
    {
      "id": "vlc-bridge-sync",
      "version": "1.2.0",
      "kind": "syncSource",
      "entry": "https://cdn.example/vlc-sync.mjs",
      "integrity": "sha384-…",
      "permissions": ["network:http://*"],
      "author": "…",
      "description": "Talks to coyote-bridge's VLC adapter"
    }
  ]
}
```

`integrity` is mandatory. `permissions` is declared up front and shown to the user before enabling —
a plugin that wants to reach the network says so.

### Host API surface (sketch)

```ts
// inside the plugin worker
declare const coyote: {
  register(def: PluginDefinition): void
  log(level: 'info'|'warn'|'error', msg: string): void
  settings: {
    schema(s: SettingsSchema): void      // host renders the UI from this
    get<T>(key: string): T
    onChange(cb: (k: string, v: unknown) => void): void
  }
  net: {
    fetch(url: string, init?: RequestInit): Promise<Response>   // permission-checked
    websocket(url: string): WebSocket                            // permission-checked
  }
}
```

Note what's absent: no Bluetooth, no direct access to the device output, no DOM. A plugin declares
values; the host decides what reaches hardware.

### Settings UI without letting plugins render

Plugins publish a JSON settings schema; the host renders it with the existing shadcn form
components. This keeps plugins from injecting UI, keeps the app visually coherent, and means a
plugin is a single small file with no build step.

## Native-side plugins (the bridge)

Some extensions genuinely need a raw socket — that's the whole reason the bridge exists. The bridge
should therefore have its own plugin story:

- **v1:** adapters compiled in, one file per player, PRs welcome. Simplest, and matches how MFP
  actually grew.
- **v2 (if demand appears):** a WASM plugin ABI on the bridge side, or subprocess adapters speaking
  a line protocol on stdio. Don't build this speculatively.

A community "VLC adapter" therefore has two halves: a bridge adapter (Rust, upstreamed) and
optionally a PWA-side settings plugin. Document that split clearly or contributors will try to
implement TCP in the browser and get frustrated.

## Open source posture

- **Licence:** MIT keeps the door open to porting MFP's (MIT) media-source code and to anyone
  embedding the core. If you want contributions back, Apache-2.0 with its patent grant, or MPL-2.0
  for file-level copyleft, are the alternatives worth a moment's thought.
- **Repo shape:** monorepo — `crates/coyote-core`, `crates/coyote-wasm`, `crates/coyote-bridge`,
  `apps/pwa`, `apps/desktop`, `plugins/`. One place to review, one CI.
- **Plugin discovery:** an official registry JSON in-repo, so adding a plugin is a PR, plus support
  for third-party registry URLs so nobody needs your permission.
- **Contribution surface:** the highest-value thing you can publish early is the **`SyncSource` and
  `InputSource` interfaces plus one reference implementation each**. That is what lets someone add
  their player without understanding the DSP.

## Risks

- **Supply chain.** Remote code execution is the entire point of a plugin system. SRI + explicit
  permissions + Worker isolation + a curated default registry is the mitigation set. Never
  auto-update a plugin without a hash change being visible.
- **Safety bypass attempts.** Someone *will* write a plugin that tries to push intensity higher than
  the UI allows. The clamps live in the WASM core, past the plugin boundary. Test this explicitly.
- **API churn.** Version the host API and refuse to load plugins declaring an incompatible major.
