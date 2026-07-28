# Capability probe

One self-contained page that answers the Phase 0 questions on real hardware. Results from the
first run are in `../07-measured-results.md`.

**Safety:** the page only ever transmits the zero-output command from
`src-tauri/src/device.rs::send_zero_command` — `B0 0F 00 00 …`. Intensity bytes are hardcoded to
zero and there is no control that can change them.

## What it checks

| Card | Question |
|---|---|
| 0 | QR of the page's own URL, so you can move it to a phone without retyping |
| 1 | Secure context, and whether `bluetooth` / `wakeLock` / `getGamepads` / `Worker` exist |
| 2 | Connect to the Coyote, enumerate every service and characteristic, confirm `150A` |
| 3 | 10 Hz loop — tick count, write count, misses, worst gap, interval p50/p95 |
| 4 | Screen Wake Lock, and visibility transitions |
| 4b | Keep-awake experiments: silent video (stops auto-lock), silent audio (survives lock) |
| 5 | Gamepad, including **analog trigger values** on buttons 6 and 7 |
| 6 | Timestamped log, and **Copy results** to dump everything as JSON |

## Running it

Web Bluetooth needs a secure context, so `http://<lan-ip>` will not work from a phone, and Bluefy
will not accept a self-signed certificate. Use a tunnel:

```bash
# terminal 1 — plain HTTP, the tunnel supplies TLS
PLAIN=1 PORT=8090 node docs/spikes/pwa/probe/serve.mjs

# terminal 2
cloudflared tunnel --url http://localhost:8090
```

Open the printed `https://….trycloudflare.com` URL. The server re-reads `index.html` on every
request, so edits show up on refresh — no rebuild, no cache fight.

For desktop-only testing, `node serve.mjs` on its own serves HTTPS with a generated self-signed
certificate on port 8443; `localhost` is a secure context so Web Bluetooth works there directly.

### It cannot run in an iframe

Web Bluetooth is gated by Permissions Policy. Inside a frame without `allow="bluetooth"`,
`navigator.bluetooth` exists and reports available, but `requestDevice()` hangs forever with no
error. Artifacts, CodePen and embedded previews are all dead ends. Top-level document only.

## Files

| File | |
|---|---|
| `index.html` | The probe. Source of truth — edit this. |
| `serve.mjs` | HTTPS (or `PLAIN=1` HTTP) server; inlines the QR library at serve time |
| `build-artifact.mjs` | Produces a single inlined file, syntax-checking the scripts |
| `vendor/qrcode.js` | qrcode-generator, MIT, vendored so the page has no CDN dependency |
