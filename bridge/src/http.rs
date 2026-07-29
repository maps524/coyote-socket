//! HTTP + WebSocket surface: serves the PWA and relays player state to it.
//!
//! One listener handles both. The request head is read into a buffer, routed
//! on, and then replayed to whichever handler wins via [`Prefixed`] — see that
//! type for why buffering rather than peeking.
//!
//! This originally used the peek-then-route trick from `src-tauri/src/net.rs`,
//! where `TcpStream::peek` leaves the bytes in the kernel buffer so the socket
//! can be handed on untouched. That is how the desktop app separates a Lovense
//! HTTP request from a T-Code WebSocket upgrade on one port. It was replaced
//! because peeking is a TCP-only facility and this surface has to work over
//! TLS: Web Bluetooth needs a secure context, and a secure page cannot open an
//! insecure socket, so the WebSocket must be able to run inside TLS or the
//! phone cannot use it at all. The routing decision is unchanged; only where
//! the bytes are held has moved.
//!
//! Either way there is no HTTP framework here, which is the point: the
//! dependency list stays a subset of what `src-tauri` already builds, and
//! these two crates are meant to merge.
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
//! - **Auth.** `/healthz` and `/ws` require a bearer token carried on the
//!   pairing URL; `/pair`, `/qr.svg` and the static app do not, because they
//!   are how a phone *obtains* the token. Read [`crate::auth`] for what that
//!   does and does not cover — in particular it is not confidentiality, since
//!   the token travels in a URL in cleartext, and it is not a substitute for
//!   TLS. It stops a page you visited from opening a WebSocket and driving
//!   your player, which is a real attack that no CORS setting prevents.
//! - **T-Code ingest.** `net.rs`'s protocol auto-detection is not carried over
//!   here; the LAN T-Code listener the desktop app provides is a separate
//!   port and a separate job.

use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::{accept_hdr_async, tungstenite::Message};

use crate::auth::{self, Token};
use crate::state::{PlayerCommand, PlayerSnapshot};
use crate::{log_debug, log_info, log_warn, qr};

pub struct Ctx {
    pub snapshot_rx: watch::Receiver<PlayerSnapshot>,
    pub cmd_tx: mpsc::Sender<PlayerCommand>,
    /// Where the PWA's `dist` lives. `None` serves a placeholder at `/`.
    pub static_dir: Option<PathBuf>,
    /// The URL to show on the pairing page — the LAN one, not `localhost`.
    /// Carries the token; see [`crate::auth`].
    pub pairing_url: String,
    /// Shared secret required on `/healthz` and `/ws`.
    ///
    /// Behind a lock because it can be rotated at runtime — see
    /// `/pair/rotate`. See [`crate::auth`] for what this does and, more
    /// importantly, what it does not do. It is not a substitute for TLS.
    pub token: std::sync::RwLock<Token>,
    /// `host:port` values a browser may legitimately claim as its `Origin`.
    pub allowed_hosts: Vec<String>,
    /// Called after a rotation so the owner can persist the new token and
    /// rebuild any URL that carries it.
    #[allow(clippy::type_complexity)]
    pub on_token_rotated: Option<Box<dyn Fn(&Token) + Send + Sync>>,
}

impl Ctx {
    pub fn token(&self) -> Token {
        self.token.read().expect("token lock").clone()
    }

    /// Replace the token and notify the owner.
    pub fn rotate_token(&self) -> Token {
        let fresh = Token::generate();
        *self.token.write().expect("token lock") = fresh.clone();
        log_info!("[http] pairing token rotated");
        if let Some(callback) = &self.on_token_rotated {
            callback(&fresh);
        }
        fresh
    }
}

/// A stream with some already-read bytes waiting in front of it.
///
/// Exists so the HTTP surface does not depend on `TcpStream::peek`. The
/// original code peeked the request head, left it in the kernel buffer, and
/// handed the raw socket to whichever handler won — which works only for TCP.
/// A TLS stream has no equivalent: by the time bytes are readable they have
/// been decrypted out of the socket and cannot be put back.
///
/// So the head is *read* into a buffer and replayed here. The routing logic is
/// unchanged; it simply now works over anything that reads and writes, which
/// is what lets the same code serve `ws://` today and `wss://` once TLS lands.
pub struct Prefixed<S> {
    head: Vec<u8>,
    consumed: usize,
    inner: S,
}

impl<S> Prefixed<S> {
    pub fn new(head: Vec<u8>, inner: S) -> Self {
        Self {
            head,
            consumed: 0,
            inner,
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Prefixed<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        // A zero-capacity poll must not be answered with "ready, nothing
        // filled" while head bytes are still pending — that is how an
        // `AsyncRead` signals EOF, so a caller polling with a full buffer
        // would conclude the stream had ended and drop the rest of the head.
        // Nothing in this crate polls that way and tungstenite does not
        // either, but this type is a building block for the TLS listener and
        // a wrapper that lies about EOF under an unusual poll is a bad thing
        // to hand someone.
        if self.consumed < self.head.len() && buf.remaining() > 0 {
            let remaining = &self.head[self.consumed..];
            let take = remaining.len().min(buf.remaining());
            buf.put_slice(&remaining[..take]);
            self.consumed += take;
            return Poll::Ready(Ok(()));
        }
        if self.consumed < self.head.len() {
            // Buffer has no room and we still owe head bytes. Pending, with an
            // immediate self-wake: returning Pending without arranging a wake
            // would hang the task forever, since no external event is coming.
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Prefixed<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// Best-guess LAN address, for the URL we hand the phone.
///
/// Uses the connected-UDP-socket trick: connecting a UDP socket sends no
/// packets, but it makes the OS pick a source address via its routing table —
/// which is exactly "the interface I would reach the network on". Avoids a
/// dependency and avoids the classic bug of picking the first interface, which
/// on a dev machine is usually a virtual adapter.
///
/// Lives here rather than in a binary because both front ends need the same
/// answer: the QR the tray shows and the QR the window shows must agree.
pub fn local_ip() -> Option<std::net::IpAddr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    // Any routable address works; nothing is sent to it.
    socket.connect("192.0.2.1:9").ok()?;
    socket.local_addr().ok().map(|a| a.ip())
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
    // Plain TCP: not a secure transport, so `secure` is false and anything
    // gated on confidentiality refuses.
    serve_conn(stream, addr, ctx, false).await
}

/// Serve one connection, over any transport.
///
/// Generic so a TLS-wrapped stream can be passed here unchanged. The head is
/// read rather than peeked (see [`Prefixed`]) because peeking is a TCP-only
/// facility, and a secure page can only open a secure socket — so the
/// WebSocket path has to work over TLS or the phone cannot use it at all.
///
/// `secure` says whether this transport is encrypted. The caller knows and
/// this function cannot find out, so it is a parameter rather than a guess.
/// It gates `/pair/rotate`, which exists to replace a token that was delivered
/// in cleartext and would be worthless if it could itself be issued in
/// cleartext.
pub async fn serve_conn<S>(stream: S, addr: SocketAddr, ctx: Arc<Ctx>, secure: bool)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut stream = stream;
    let head = match read_request_head(&mut stream).await {
        Ok(Head::Complete(head)) => head,
        Ok(Head::TooLarge) => {
            // Say so, on the wire and in the log. The previous behaviour —
            // stop reading, write, close on a still-full socket — sent RST and
            // destroyed the response in flight, so the phone saw an
            // unexplained failure and there was nothing on either side to
            // explain it.
            log_warn!("[http] {addr} request head exceeded {MAX_HEAD_BYTES} bytes");
            drain_briefly(&mut stream).await;
            let _ = respond(
                &mut stream,
                431,
                "text/plain; charset=utf-8",
                b"request header fields too large",
            )
            .await;
            return;
        }
        // Nothing usable arrived. A port scan and a client that hung up
        // mid-request both land here, and neither is worth a response.
        Ok(Head::Incomplete) | Err(_) => return,
    };

    if is_websocket_upgrade(&head) {
        // The token is on the query string of the upgrade request, and the
        // Origin is in its headers. Both are checked before the handshake
        // completes: a browser that is refused should see a failed connection,
        // not an open socket that goes quiet.
        let target = parse_request_line(&head).map(|(_, path)| path).unwrap_or("");
        let origin = header_value(&head, "origin");
        if !authorised(target, origin, &ctx) {
            log_warn!("[ws] {addr} refused: bad or missing token");
            let mut stream = Prefixed::new(head, stream);
            let _ = respond(&mut stream, 401, "text/plain; charset=utf-8", b"unauthorized").await;
            return;
        }

        let stream = Prefixed::new(head, stream);
        match accept_hdr_async(stream, |_req: &_, res| Ok(res)).await {
            Ok(ws) => ws_relay(ws, addr, ctx).await,
            Err(e) => log_warn!("[http] {addr} websocket handshake failed: {e}"),
        }
        return;
    }

    let Some((method, path)) = parse_request_line(&head).map(|(m, p)| (m.to_string(), p.to_string()))
    else {
        let mut stream = Prefixed::new(head, stream);
        let _ = respond(&mut stream, 400, "text/plain; charset=utf-8", b"bad request").await;
        return;
    };
    // Log the path *without* its query string. The query carries the pairing
    // token, and this line reaches the ring buffer, the log file, stderr and
    // the broadcast channel the desktop window renders — which is also what
    // the "Copy all" button assembles for pasting into a bug report. Logging
    // the full target turned a credential into something the UI actively
    // encourages users to hand out.
    log_debug!("[http] {addr} {method} {}", redact_query(&path));

    let origin = header_value(&head, "origin").map(|o| o.to_string());
    let mut stream = Prefixed::new(head, stream);

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

    route(&mut stream, &path, origin.as_deref(), secure, &ctx).await;
}

/// Strip the query string from a request target before it is logged.
///
/// Everything secret in a request to this server lives in the query. Rather
/// than redacting the token by name — which fails the moment a second secret
/// parameter appears — drop the query wholesale and keep a marker so a reader
/// can tell "no query" from "query withheld".
fn redact_query(target: &str) -> String {
    match target.split_once('?') {
        Some((path, _)) => format!("{path}?<redacted>"),
        None => target.to_string(),
    }
}

/// Whether a request carries a valid token and an acceptable `Origin`.
fn authorised(path_and_query: &str, origin: Option<&str>, ctx: &Ctx) -> bool {
    let presented = auth::token_from_query(path_and_query);
    presented.is_some_and(|t| ctx.token().matches(t))
        && auth::origin_is_acceptable(origin, &ctx.allowed_hosts)
}

async fn route<W>(stream: &mut W, path: &str, origin: Option<&str>, secure: bool, ctx: &Ctx)
where
    W: AsyncWrite + Unpin,
{
    let full = path;
    // Strip the query string for routing; the token lives in it.
    let path = path.split('?').next().unwrap_or("/");

    let result = match path {
        // Gated: leaks the media URL, the LAN address and the link state.
        "/healthz" => {
            if !authorised(full, origin, ctx) {
                respond(stream, 401, "text/plain; charset=utf-8", b"unauthorized").await
            } else {
                let snap = ctx.snapshot_rx.borrow().clone();
                let body = serde_json::to_vec_pretty(&snap).unwrap_or_default();
                respond(stream, 200, "application/json; charset=utf-8", &body).await
            }
        }
        // Ungated, deliberately. The pairing page and its QR are how a phone
        // *obtains* the token, so gating them on the token is a bootstrap
        // that cannot start. They are reachable only from the LAN and they
        // hand out a credential — which is a real exposure, and the reason the
        // pairing page says so and the tray offers rotation.
        "/pair" => {
            let body = pairing_page(&ctx.pairing_url);
            respond(stream, 200, "text/html; charset=utf-8", body.as_bytes()).await
        }
        // Exchange a token that travelled in cleartext for one that did not.
        //
        // The pairing QR must point at plain HTTP: a phone that has not yet
        // trusted the local CA meets a full-page certificate interstitial with
        // no route back to the instructions. So the first token is always
        // sniffable by anyone on the LAN at that moment. This narrows the
        // window from "forever" to "until the phone finishes pairing" — it
        // does not close it, and a sniffer who acts within that window can
        // rotate the token themselves and lock the user out.
        //
        // Refused over plaintext, because a rotation delivered in cleartext
        // would hand the eavesdropper the replacement too.
        "/pair/rotate" => {
            if !secure {
                respond(
                    stream,
                    403,
                    "text/plain; charset=utf-8",
                    b"rotation requires a secure transport",
                )
                .await
            } else if !authorised(full, origin, ctx) {
                respond(stream, 401, "text/plain; charset=utf-8", b"unauthorized").await
            } else {
                let fresh = ctx.rotate_token();
                let body = serde_json::json!({ "token": fresh.as_str() }).to_string();
                respond(
                    stream,
                    200,
                    "application/json; charset=utf-8",
                    body.as_bytes(),
                )
                .await
            }
        }
        "/qr.svg" => match qr::to_svg(&ctx.pairing_url) {
            Ok(svg) => respond(stream, 200, "image/svg+xml; charset=utf-8", svg.as_bytes()).await,
            Err(e) => {
                let msg = format!("could not render QR: {e}");
                respond(stream, 500, "text/plain; charset=utf-8", msg.as_bytes()).await
            }
        },
        // Ungated: static assets are the app itself, which has to load before
        // it can present anything. It reads the token from its own URL and
        // uses it for `/ws`, which is where the capability actually is.
        _ => serve_static(stream, path, ctx).await,
    };

    if let Err(e) = result {
        log_debug!("[http] response write failed: {e}");
    }
}

async fn serve_static<W>(stream: &mut W, path: &str, ctx: &Ctx) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
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

pub(crate) async fn respond<W>(
    stream: &mut W,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        431 => "Request Header Fields Too Large",
        _ => "Internal Server Error",
    };
    // No `Access-Control-Allow-Origin: *`. It was there to make a browser on
    // another origin able to read these responses, which is precisely the
    // thing that should not happen: it let any page in any tab read `/healthz`
    // and learn the media URL and LAN address. Same-origin requests do not
    // need the header, and nothing legitimate here is cross-origin.
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\n\
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
pub(crate) async fn ws_relay<S>(
    ws: tokio_tungstenite::WebSocketStream<S>,
    addr: SocketAddr,
    ctx: Arc<Ctx>,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
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

/// Cap on the request head, so a peer that never sends `\r\n\r\n` cannot make
/// us buffer without limit.
const MAX_HEAD_BYTES: usize = 16 * 1024;

/// How reading the request head ended.
///
/// Three of these are not success, and an earlier version returned all of them
/// as `Ok(head)`. A truncated head with a parseable request line was then
/// routed normally — and any header that had not arrived, including `Origin`,
/// simply appeared absent. `origin_is_acceptable(None, …)` deliberately allows
/// a missing `Origin` so native clients work, so a phone on flaky Wi-Fi whose
/// head straddled a stall was served on half its headers. The token was still
/// required, so this was never an auth bypass, but "served on partial
/// headers" is not a state worth having, and its likely symptom was a
/// mysterious 401 blamed on the token.
enum Head {
    /// Terminated by `\r\n\r\n`. The only complete outcome.
    Complete(Vec<u8>),
    /// The peer closed, or said nothing at all, before finishing.
    Incomplete,
    /// Larger than [`MAX_HEAD_BYTES`]. Distinguishable because it deserves a
    /// different status code and a log line.
    TooLarge,
}

/// Read until the header block is complete.
///
/// Replaces the `TcpStream::peek` version lifted from `net.rs`. Peeking left
/// the bytes in the kernel buffer so the socket could be handed to a handler
/// untouched — elegant, and unavailable on a TLS stream, where readable bytes
/// have already been decrypted out of the socket and cannot be put back. The
/// head is buffered instead and replayed through [`Prefixed`], which costs one
/// small allocation per request and works over any transport.
async fn read_request_head<S>(stream: &mut S) -> std::io::Result<Head>
where
    S: AsyncRead + Unpin,
{
    use std::time::Duration;

    let mut head = Vec::with_capacity(1024);
    let mut byte = [0u8; 1];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);

    loop {
        // Byte at a time: the head must not be over-read, because everything
        // after it belongs to the body or to the WebSocket framing.
        match tokio::time::timeout_at(deadline, stream.read(&mut byte)).await {
            Ok(Ok(0)) => return Ok(Head::Incomplete),
            Ok(Ok(_)) => head.push(byte[0]),
            Ok(Err(e)) => return Err(e),
            Err(_) => return Ok(Head::Incomplete),
        }
        if head.ends_with(b"\r\n\r\n") {
            return Ok(Head::Complete(head));
        }
        if head.len() >= MAX_HEAD_BYTES {
            return Ok(Head::TooLarge);
        }
    }
}

/// Read and discard whatever the peer is still sending, briefly.
///
/// Closing a socket with unread data queued makes the OS send RST, which
/// destroys any response already written — so a client that sent a 32 KB head
/// got an empty reply and a connection reset, with nothing logged on either
/// side to explain it. Draining first lets the response actually arrive.
/// Bounded, because the point is to be polite, not to read an unbounded body.
async fn drain_briefly<S>(stream: &mut S)
where
    S: AsyncRead + Unpin,
{
    use std::time::Duration;
    let mut sink = vec![0u8; 4096];
    let deadline = tokio::time::Instant::now() + Duration::from_millis(250);
    loop {
        match tokio::time::timeout_at(deadline, stream.read(&mut sink)).await {
            Ok(Ok(0)) | Ok(Err(_)) | Err(_) => return,
            Ok(Ok(_)) => continue,
        }
    }
}

/// Case-insensitive header lookup on a raw request head.
pub fn header_value<'a>(head: &'a [u8], name: &str) -> Option<&'a str> {
    let text = std::str::from_utf8(head).ok()?;
    text.split("\r\n").skip(1).find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
    })
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

    /// Only `\r\n\r\n` means complete. An earlier version returned every
    /// outcome as success, so a truncated head was routed on whatever headers
    /// had happened to arrive — and a missing `Origin` reads as "native
    /// client", which is allowed.
    #[tokio::test]
    async fn an_incomplete_head_is_not_mistaken_for_a_complete_one() {
        use tokio::io::duplex;
        use tokio::io::AsyncWriteExt;

        let (mut peer, mut server) = duplex(4096);
        peer.write_all(b"GET /healthz?t=x HTTP/1.1\r\nHost: a\r\nOrig")
            .await
            .unwrap();
        drop(peer); // EOF mid-header

        assert!(matches!(
            read_request_head(&mut server).await.unwrap(),
            Head::Incomplete
        ));
    }

    #[tokio::test]
    async fn a_complete_head_is_returned_whole() {
        use tokio::io::duplex;
        use tokio::io::AsyncWriteExt;

        let (mut peer, mut server) = duplex(4096);
        peer.write_all(b"GET / HTTP/1.1\r\nHost: a\r\nOrigin: http://a\r\n\r\n")
            .await
            .unwrap();

        let Head::Complete(head) = read_request_head(&mut server).await.unwrap() else {
            panic!("should be complete");
        };
        assert_eq!(header_value(&head, "origin"), Some("http://a"));
    }

    /// A head split across writes must reassemble, because that is what a
    /// phone on flaky Wi-Fi looks like.
    #[tokio::test]
    async fn a_head_split_across_writes_reassembles() {
        use tokio::io::duplex;
        use tokio::io::AsyncWriteExt;

        let (mut peer, mut server) = duplex(4096);
        tokio::spawn(async move {
            for chunk in [
                &b"GET /healthz"[..],
                &b"?t=abc HTTP/1.1\r\nHost: a\r\n"[..],
                &b"Origin: http://a\r\n\r\n"[..],
            ] {
                peer.write_all(chunk).await.unwrap();
                tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            }
            std::future::pending::<()>().await;
        });

        let Head::Complete(head) = read_request_head(&mut server).await.unwrap() else {
            panic!("a split head must still be complete");
        };
        assert_eq!(parse_request_line(&head), Some(("GET", "/healthz?t=abc")));
        assert_eq!(header_value(&head, "origin"), Some("http://a"));
    }

    #[tokio::test]
    async fn an_oversized_head_is_distinguishable() {
        use tokio::io::duplex;
        use tokio::io::AsyncWriteExt;

        let (mut peer, mut server) = duplex(64 * 1024);
        tokio::spawn(async move {
            let _ = peer.write_all(b"GET / HTTP/1.1\r\n").await;
            // Never terminated.
            let junk = vec![b'x'; MAX_HEAD_BYTES + 1024];
            let _ = peer.write_all(&junk).await;
            std::future::pending::<()>().await;
        });

        assert!(matches!(
            read_request_head(&mut server).await.unwrap(),
            Head::TooLarge
        ));
    }

    /// The head must survive being read through a buffer smaller than itself,
    /// which is how a real reader consumes it.
    #[tokio::test]
    async fn prefixed_replays_the_head_through_small_reads() {
        use tokio::io::duplex;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (mut peer, server) = duplex(4096);
        peer.write_all(b"TAIL").await.unwrap();
        drop(peer);

        let mut prefixed = Prefixed::new(b"HEADHEAD".to_vec(), server);
        let mut out = Vec::new();
        let mut chunk = [0u8; 3];
        loop {
            let n = prefixed.read(&mut chunk).await.unwrap();
            if n == 0 {
                break;
            }
            out.extend_from_slice(&chunk[..n]);
        }
        assert_eq!(out, b"HEADHEADTAIL");
    }

    #[test]
    fn logged_paths_carry_no_query_string() {
        // The query is where the token lives, and this line reaches a log the
        // UI offers to copy.
        assert_eq!(redact_query("/healthz?t=secret"), "/healthz?<redacted>");
        assert_eq!(redact_query("/pair"), "/pair");
        assert!(!redact_query("/ws?t=secret&x=1").contains("secret"));
    }

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
