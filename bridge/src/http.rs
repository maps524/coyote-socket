//! HTTP + WebSocket surface: serves the PWA and relays player state to it.
//!
//! One `TcpListener` handles both, using the peek-then-route trick lifted from
//! `src-tauri/src/net.rs` — `TcpStream::peek` does not advance the read
//! pointer, so the bytes are still there for whichever handler wins. That is
//! how the desktop app already separates a Lovense HTTP request from a T-Code
//! WebSocket upgrade on one port, and it is the reason this needs no HTTP
//! framework: the dependency list stays a subset of what `src-tauri` already
//! builds, which matters because these two crates are meant to merge.
//!
//! Serving the PWA from the bridge (rather than hosting it centrally) is a
//! decision from the spike record: no version skew between bridge and app,
//! users can modify their own instance, no central dependency.
//!
//! ## Seams deliberately left open
//!
//! - **TLS.** Everything here is plaintext HTTP. Web Bluetooth requires a
//!   secure context, so the phone will need `https://` — via a certificate or
//!   a tunnel — before this is usable for real. `localhost` is exempt, which
//!   is why the desktop-browser path works today and the phone path does not.
//! - **Auth.** Anything on the LAN can connect. Fine for a spike, not for a
//!   thing that drives hardware.
//! - **T-Code ingest.** `net.rs`'s protocol auto-detection is not carried over
//!   here; the LAN T-Code listener the desktop app provides is a separate
//!   port and a separate job.

use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::{accept_async, tungstenite::Message};

use crate::state::{PlayerCommand, PlayerSnapshot};
use crate::{log_debug, log_info, log_warn, qr};

pub struct Ctx {
    pub snapshot_rx: watch::Receiver<PlayerSnapshot>,
    pub cmd_tx: mpsc::Sender<PlayerCommand>,
    /// Where the PWA's `dist` lives. `None` serves a placeholder at `/`.
    pub static_dir: Option<PathBuf>,
    /// The URL to show on the pairing page — the LAN one, not `localhost`.
    pub pairing_url: String,
}

pub async fn run(listener: TcpListener, ctx: Arc<Ctx>) {
    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                let ctx = Arc::clone(&ctx);
                tokio::spawn(async move { handle(stream, addr, ctx).await });
            }
            Err(e) => {
                log_warn!("[http] accept failed: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
        }
    }
}

async fn handle(stream: TcpStream, addr: SocketAddr, ctx: Arc<Ctx>) {
    let mut peek_buf = vec![0u8; 2048];
    let n = match peek_request_head(&stream, &mut peek_buf).await {
        Ok(0) | Err(_) => return,
        Ok(n) => n,
    };
    let head = &peek_buf[..n];

    if is_websocket_upgrade(head) {
        match accept_async(stream).await {
            Ok(ws) => ws_relay(ws, addr, ctx).await,
            Err(e) => log_warn!("[http] {addr} websocket handshake failed: {e}"),
        }
        return;
    }

    // Consume the bytes we peeked so the socket is positioned past the
    // request head before we reply.
    let mut stream = stream;
    let mut sink = vec![0u8; n];
    if stream.read_exact(&mut sink).await.is_err() {
        return;
    }

    let Some((method, path)) = parse_request_line(head) else {
        let _ = respond(
            &mut stream,
            400,
            "text/plain; charset=utf-8",
            b"bad request",
        )
        .await;
        return;
    };
    log_debug!("[http] {addr} {method} {path}");

    if method != "GET" && method != "HEAD" {
        let _ = respond(
            &mut stream,
            405,
            "text/plain; charset=utf-8",
            b"method not allowed",
        )
        .await;
        return;
    }

    route(&mut stream, path, &ctx).await;
}

async fn route(stream: &mut TcpStream, path: &str, ctx: &Ctx) {
    // Strip the query string; none of these routes take parameters yet.
    let path = path.split('?').next().unwrap_or("/");

    let result = match path {
        "/healthz" => {
            let snap = ctx.snapshot_rx.borrow().clone();
            let body = serde_json::to_vec_pretty(&snap).unwrap_or_default();
            respond(stream, 200, "application/json; charset=utf-8", &body).await
        }
        "/pair" => {
            let body = pairing_page(&ctx.pairing_url);
            respond(stream, 200, "text/html; charset=utf-8", body.as_bytes()).await
        }
        "/qr.svg" => match qr::to_svg(&ctx.pairing_url) {
            Ok(svg) => respond(stream, 200, "image/svg+xml; charset=utf-8", svg.as_bytes()).await,
            Err(e) => {
                let msg = format!("could not render QR: {e}");
                respond(stream, 500, "text/plain; charset=utf-8", msg.as_bytes()).await
            }
        },
        _ => serve_static(stream, path, ctx).await,
    };

    if let Err(e) = result {
        log_debug!("[http] response write failed: {e}");
    }
}

async fn serve_static(stream: &mut TcpStream, path: &str, ctx: &Ctx) -> std::io::Result<()> {
    let Some(root) = ctx.static_dir.as_ref() else {
        let body = placeholder_page(&ctx.pairing_url);
        return respond(stream, 200, "text/html; charset=utf-8", body.as_bytes()).await;
    };

    let Some(rel) = safe_relative_path(path) else {
        return respond(stream, 403, "text/plain; charset=utf-8", b"forbidden").await;
    };

    let mut file = root.join(&rel);
    if file.is_dir() {
        file = file.join("index.html");
    }

    match tokio::fs::read(&file).await {
        Ok(bytes) => respond(stream, 200, mime_for(&file), &bytes).await,
        Err(_) => {
            // SPA fallback: unknown paths get index.html so client-side
            // routing works on a hard refresh.
            match tokio::fs::read(root.join("index.html")).await {
                Ok(bytes) => respond(stream, 200, "text/html; charset=utf-8", &bytes).await,
                Err(_) => respond(stream, 404, "text/plain; charset=utf-8", b"not found").await,
            }
        }
    }
}

/// Reject anything that could escape the static root: absolute paths, `..`,
/// Windows drive prefixes, UNC roots.
fn safe_relative_path(url_path: &str) -> Option<PathBuf> {
    let trimmed = url_path.trim_start_matches('/');
    if trimmed.is_empty() {
        return Some(PathBuf::from("index.html"));
    }
    let candidate = Path::new(trimmed);
    let mut out = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::Normal(part) => out.push(part),
            // Everything else is either an escape attempt or meaningless here.
            _ => return None,
        }
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

fn mime_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("svg") => "image/svg+xml; charset=utf-8",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("webmanifest") => "application/manifest+json",
        // Funscripts are JSON; the PWA fetches them, so they need a sane type.
        Some("funscript") => "application/json; charset=utf-8",
        _ => "application/octet-stream",
    }
}

async fn respond(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Internal Server Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await
}

// ---------------------------------------------------------------------------
// WebSocket relay
// ---------------------------------------------------------------------------

/// Relay player state to one client, and accept commands back.
///
/// The message format is a stub, but an honest one: it carries exactly the
/// four things the spike promised — position, playing/paused, duration and
/// file identity — plus the link state, because "the bridge cannot see the
/// player" is a thing the phone has to render differently from "paused".
async fn ws_relay(
    ws: tokio_tungstenite::WebSocketStream<TcpStream>,
    addr: SocketAddr,
    ctx: Arc<Ctx>,
) {
    log_info!("[ws] {addr} connected");
    let (mut tx, mut rx) = ws.split();
    let mut snapshots = ctx.snapshot_rx.clone();

    // Tell the client what it is talking to before any state arrives.
    let hello = serde_json::json!({
        "type": "hello",
        "bridge": env!("CARGO_PKG_NAME"),
        "version": env!("CARGO_PKG_VERSION"),
        "carries": ["position", "playing", "duration", "media"],
        "accepts": ["seek", "play", "pause"],
        "note": "spike build — message shape is not stable",
    });
    if tx.send(Message::Text(hello.to_string())).await.is_err() {
        return;
    }

    // Send current state immediately; a reconnecting phone must not wait for
    // the next change to learn where playback is.
    let initial = snapshots.borrow_and_update().clone();
    if send_snapshot(&mut tx, &initial).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            changed = snapshots.changed() => {
                if changed.is_err() {
                    break; // bridge shutting down
                }
                let snap = snapshots.borrow_and_update().clone();
                if send_snapshot(&mut tx, &snap).await.is_err() {
                    break;
                }
            }
            incoming = rx.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        if let Some(cmd) = parse_client_command(&text) {
                            // Never block the relay on a full queue: dropping a
                            // stale seek is better than stalling state updates.
                            if ctx.cmd_tx.try_send(cmd).is_err() {
                                log_warn!("[ws] command dropped — queue full or player task gone");
                            }
                        } else {
                            log_debug!("[ws] {addr} sent an unrecognised message: {text}");
                        }
                    }
                    Some(Ok(Message::Ping(p))) => {
                        if tx.send(Message::Pong(p)).await.is_err() { break; }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(e)) => {
                        log_debug!("[ws] {addr} error: {e}");
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    log_info!("[ws] {addr} disconnected");
}

async fn send_snapshot<S>(tx: &mut S, snap: &PlayerSnapshot) -> Result<(), ()>
where
    S: SinkExt<Message> + Unpin,
{
    let json = serde_json::to_string(snap).map_err(|_| ())?;
    tx.send(Message::Text(json)).await.map_err(|_| ())
}

/// Translate a client message into a player command.
///
/// Kept tiny and tolerant: the phone→player direction is a bonus here, not
/// something the spike had to prove.
pub fn parse_client_command(text: &str) -> Option<PlayerCommand> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    match v.get("type")?.as_str()? {
        "seek" => Some(PlayerCommand {
            current_time: Some(v.get("positionS")?.as_f64()?),
            ..Default::default()
        }),
        // 0 = playing, 1 = paused, per the player protocol.
        "play" => Some(PlayerCommand {
            player_state: Some(0),
            ..Default::default()
        }),
        "pause" => Some(PlayerCommand {
            player_state: Some(1),
            ..Default::default()
        }),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Request parsing
// ---------------------------------------------------------------------------

/// Peek until the header block is complete or the buffer fills. Lifted from
/// `net.rs::peek_request_head`.
async fn peek_request_head(stream: &TcpStream, buf: &mut [u8]) -> std::io::Result<usize> {
    use std::time::Duration;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut last_len = 0usize;

    loop {
        let now = std::time::Instant::now();
        if now >= deadline {
            return Ok(last_len);
        }
        let n = match tokio::time::timeout(deadline - now, stream.peek(buf)).await {
            Ok(Ok(n)) => n,
            Ok(Err(e)) => return Err(e),
            Err(_) => return Ok(last_len),
        };
        if n == 0 {
            return Ok(last_len);
        }
        if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") || n == buf.len() {
            return Ok(n);
        }
        if n == last_len {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        last_len = n;
    }
}

pub fn is_websocket_upgrade(head: &[u8]) -> bool {
    let text = String::from_utf8_lossy(head).to_ascii_lowercase();
    text.contains("upgrade: websocket") || text.contains("sec-websocket-key:")
}

/// Split `GET /path HTTP/1.1` into method and path.
pub fn parse_request_line(head: &[u8]) -> Option<(&str, &str)> {
    let line_end = head.windows(2).position(|w| w == b"\r\n")?;
    let line = std::str::from_utf8(&head[..line_end]).ok()?;
    let mut parts = line.split(' ');
    let method = parts.next()?;
    let path = parts.next()?;
    if method.is_empty() || !path.starts_with('/') {
        return None;
    }
    Some((method, path))
}

// ---------------------------------------------------------------------------
// Built-in pages
// ---------------------------------------------------------------------------

fn page_shell(title: &str, body: &str) -> String {
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{title}</title>
<style>
:root {{ color-scheme: light dark; }}
body {{ font: 16px/1.5 system-ui, sans-serif; margin: 0; min-height: 100vh;
        display: grid; place-items: center; padding: 2rem; }}
main {{ max-width: 30rem; text-align: center; }}
img {{ width: min(70vw, 18rem); aspect-ratio: 1; background: #fff;
       border-radius: .5rem; padding: .5rem; }}
code {{ font-size: 1.05rem; word-break: break-all;
        background: color-mix(in srgb, currentColor 12%, transparent);
        padding: .15rem .4rem; border-radius: .25rem; }}
p.hint {{ opacity: .7; font-size: .9rem; }}
</style></head><body><main>{body}</main></body></html>"#
    )
}

fn pairing_page(url: &str) -> String {
    page_shell(
        "Open on your phone",
        &format!(
            r#"<h1>Open this on your phone</h1>
<img src="/qr.svg" alt="QR code for {url}">
<p><code>{url}</code></p>
<p class="hint">Both devices must be on the same network.</p>"#
        ),
    )
}

fn placeholder_page(url: &str) -> String {
    page_shell(
        "Bridge running",
        &format!(
            r#"<h1>Bridge is running</h1>
<p>No app has been mounted. Point <code>--static-dir</code> at the PWA's
<code>dist</code> folder and reload.</p>
<p><a href="/pair">Pairing QR</a> &middot; <a href="/healthz">Status JSON</a></p>
<p class="hint">Phone URL: <code>{url}</code></p>"#
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_request_line() {
        let head = b"GET /index.html HTTP/1.1\r\nHost: x\r\n\r\n";
        assert_eq!(parse_request_line(head), Some(("GET", "/index.html")));
    }

    #[test]
    fn rejects_a_garbage_request_line() {
        assert_eq!(parse_request_line(b"nonsense\r\n\r\n"), None);
        assert_eq!(
            parse_request_line(b"GET nonabsolute HTTP/1.1\r\n\r\n"),
            None
        );
    }

    #[test]
    fn detects_a_websocket_upgrade_case_insensitively() {
        assert!(is_websocket_upgrade(
            b"GET /ws HTTP/1.1\r\nUpgrade: WebSocket\r\n\r\n"
        ));
        assert!(is_websocket_upgrade(
            b"GET /ws HTTP/1.1\r\nSec-WebSocket-Key: abc\r\n\r\n"
        ));
        assert!(!is_websocket_upgrade(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n"));
    }

    #[test]
    fn root_maps_to_index() {
        assert_eq!(safe_relative_path("/"), Some(PathBuf::from("index.html")));
        assert_eq!(safe_relative_path(""), Some(PathBuf::from("index.html")));
    }

    #[test]
    fn rejects_directory_traversal() {
        for evil in [
            "/../secrets.txt",
            "/assets/../../etc/passwd",
            "/..",
            "//C:/Windows/win.ini",
        ] {
            assert_eq!(safe_relative_path(evil), None, "should reject {evil}");
        }
    }

    #[test]
    fn accepts_ordinary_nested_assets() {
        assert_eq!(
            safe_relative_path("/assets/app-1a2b.js"),
            Some(PathBuf::from("assets").join("app-1a2b.js"))
        );
    }

    #[test]
    fn wasm_and_funscript_get_useful_mime_types() {
        assert_eq!(mime_for(Path::new("a.wasm")), "application/wasm");
        assert_eq!(
            mime_for(Path::new("clip.funscript")),
            "application/json; charset=utf-8"
        );
    }

    #[test]
    fn client_commands_map_to_player_fields() {
        let seek = parse_client_command(r#"{"type":"seek","positionS":42.5}"#).unwrap();
        assert_eq!(seek.current_time, Some(42.5));
        assert_eq!(
            parse_client_command(r#"{"type":"pause"}"#)
                .unwrap()
                .player_state,
            Some(1)
        );
        assert_eq!(
            parse_client_command(r#"{"type":"play"}"#)
                .unwrap()
                .player_state,
            Some(0)
        );
        assert!(parse_client_command(r#"{"type":"launch-missiles"}"#).is_none());
        assert!(parse_client_command("not json").is_none());
    }
}
