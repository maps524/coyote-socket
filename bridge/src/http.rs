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
//! - **Auth.** `/healthz` and `/ws` require a bearer token carried on the
//!   pairing URL; `/pair`, `/qr.svg`, `/install`, `/ca.crt` and the static app
//!   do not, because they are how a phone *obtains* the token or the means to
//!   connect at all. Read [`crate::auth`] for what that does and does not
//!   cover — in particular it is not confidentiality, since the token travels
//!   in a URL in cleartext, and it is not a substitute for TLS. It stops a page
//!   you visited from opening a WebSocket and driving your player, which is a
//!   real attack that no CORS setting prevents.
//! - **T-Code ingest.** `net.rs`'s protocol auto-detection is not carried over
//!   here; the LAN T-Code listener the desktop app provides is a separate
//!   port and a separate job.
//!
//! ## TLS is no longer one of them
//!
//! This module is transport-agnostic. The same routing table serves the plain
//! listener and the TLS one ([`crate::tls`]), which is what makes `wss://`
//! possible — and `wss://` is not optional, because a page served over HTTPS
//! is forbidden from opening a `ws://` socket. A secure origin that could not
//! open its own relay would load the app and then be unable to talk to it.
//!
//! TLS and the token are two halves answering different attackers, and neither
//! covers the other's gap: TLS gives confidentiality and no authorization, the
//! token gives authorization and no confidentiality. Nothing here should be
//! read as "the bridge is secure".

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
use crate::library::{self, Library};
use crate::state::{PlayerCommand, PlayerSnapshot};
use crate::{log_debug, log_info, log_warn, qr};

pub struct Ctx {
    pub snapshot_rx: watch::Receiver<PlayerSnapshot>,
    pub cmd_tx: mpsc::Sender<PlayerCommand>,
    /// Where the PWA's `dist` lives. `None` serves a placeholder at `/`.
    pub static_dir: Option<PathBuf>,
    /// The funscript library, if one is configured.
    ///
    /// `None` is a normal state, not an error: `/library/index.json` answers
    /// 200 with an empty list and `configured: false`, so the app can say "you
    /// have not pointed me at a folder" rather than showing a failure. See
    /// [`crate::library`].
    pub library: Option<Arc<Library>>,
    /// The URL to show on the pairing page — the LAN one, not `localhost` —
    /// **without** the token.
    ///
    /// Stored without it and composed on demand by [`Ctx::pairing_url`]. It
    /// used to hold the finished URL, which was a latent bug: rotation
    /// replaces the token but could not replace a string captured at startup,
    /// so `/pair` and `/qr.svg` would have gone on serving a QR encoding the
    /// token that had just been revoked. Nothing called rotation yet, so it had
    /// never fired — deriving it removes the possibility rather than the
    /// symptom.
    pub pairing_base: String,
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
    /// Public TLS material, when a certificate has been issued.
    ///
    /// `None` when the bridge is serving plain HTTP only, in which case
    /// `/install` says there is nothing to install rather than 404ing — a
    /// disabled feature and a missing page are different answers to give
    /// someone who is trying to work out why their phone will not connect.
    ///
    /// Holds no key material of any kind; see [`crate::install::TlsPublicInfo`].
    pub tls: Option<Arc<crate::install::TlsPublicInfo>>,
}

impl Ctx {
    pub fn token(&self) -> Token {
        self.token.read().expect("token lock").clone()
    }

    /// The full pairing URL, with whatever token is current.
    pub fn pairing_url(&self) -> String {
        auth::with_token(&self.pairing_base, &self.token())
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
                Status::HEADERS_TOO_LARGE,
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
        let target = parse_request_line(&head)
            .map(|(_, path)| path)
            .unwrap_or("");
        let origin = header_value(&head, "origin");
        if !authorised(target, origin, &ctx) {
            log_warn!("[ws] {addr} refused: bad or missing token");
            let mut stream = Prefixed::new(head, stream);
            let _ = respond(
                &mut stream,
                Status::UNAUTHORIZED,
                "text/plain; charset=utf-8",
                b"unauthorized",
            )
            .await;
            return;
        }

        let stream = Prefixed::new(head, stream);
        match accept_hdr_async(stream, |_req: &_, res| Ok(res)).await {
            Ok(ws) => ws_relay(ws, addr, ctx).await,
            Err(e) => log_warn!("[http] {addr} websocket handshake failed: {e}"),
        }
        return;
    }

    let Some((method, path)) =
        parse_request_line(&head).map(|(m, p)| (m.to_string(), p.to_string()))
    else {
        let mut stream = Prefixed::new(head, stream);
        let _ = respond(
            &mut stream,
            Status::BAD_REQUEST,
            "text/plain; charset=utf-8",
            b"bad request",
        )
        .await;
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
            Status::METHOD_NOT_ALLOWED,
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
                respond(
                    stream,
                    Status::UNAUTHORIZED,
                    "text/plain; charset=utf-8",
                    b"unauthorized",
                )
                .await
            } else {
                let snap = ctx.snapshot_rx.borrow().clone();
                let body = serde_json::to_vec_pretty(&snap).unwrap_or_default();
                respond(stream, Status::OK, "application/json; charset=utf-8", &body).await
            }
        }
        // Ungated, deliberately. The pairing page and its QR are how a phone
        // *obtains* the token, so gating them on the token is a bootstrap
        // that cannot start. They are reachable only from the LAN and they
        // hand out a credential — which is a real exposure, and the reason the
        // pairing page says so and the tray offers rotation.
        "/pair" => {
            let body = pairing_page(&ctx.pairing_url());
            respond(
                stream,
                Status::OK,
                "text/html; charset=utf-8",
                body.as_bytes(),
            )
            .await
        }
        // Revoke the current token and issue a new one.
        //
        // **This is deliberately not called by the pairing flow**, and the
        // reason is worth stating because the opposite looked obviously right.
        //
        // The original idea was to rotate automatically once the phone had
        // trusted the CA, exchanging a token that travelled in cleartext for
        // one that did not. That works for one device and breaks the moment
        // there are two. There is a single shared token, so rotating it
        // un-pairs *everything* — pair a tablet and the phone stops working,
        // with no message, and the symptom arrives hours later as "the app
        // stopped connecting". A household with a phone and a tablet is not an
        // exotic case, and silently breaking it is worse than the exposure the
        // rotation was closing: a LAN eavesdropper present during the few
        // seconds of pairing.
        //
        // So rotation stays a deliberate act with an understood consequence —
        // "revoke everything and re-pair" — surfaced in the UI rather than
        // fired as a side effect. It is the right tool for "I showed someone
        // the QR" or "I pasted a URL I should not have", which are the
        // situations people actually find themselves in.
        //
        // Refused over plaintext, because a replacement token delivered in
        // cleartext hands the eavesdropper the replacement too.
        "/pair/rotate" => {
            if !secure {
                respond(
                    stream,
                    Status::FORBIDDEN,
                    "text/plain; charset=utf-8",
                    b"rotation requires a secure transport",
                )
                .await
            } else if !authorised(full, origin, ctx) {
                respond(
                    stream,
                    Status::UNAUTHORIZED,
                    "text/plain; charset=utf-8",
                    b"unauthorized",
                )
                .await
            } else {
                let fresh = ctx.rotate_token();
                let body = serde_json::json!({ "token": fresh.as_str() }).to_string();
                respond(
                    stream,
                    Status::OK,
                    "application/json; charset=utf-8",
                    body.as_bytes(),
                )
                .await
            }
        }
        // Gated, on the same reasoning as `/healthz`: the listing names every
        // file in a directory the user chose, which is as personal as the media
        // URL that endpoint already protects.
        p if p.starts_with("/library/") => {
            if !authorised(full, origin, ctx) {
                respond(
                    stream,
                    Status::UNAUTHORIZED,
                    "text/plain; charset=utf-8",
                    b"unauthorized",
                )
                .await
            } else {
                serve_library(stream, p, ctx).await
            }
        }
        "/qr.svg" => match qr::to_svg(&ctx.pairing_url()) {
            Ok(svg) => {
                respond(
                    stream,
                    Status::OK,
                    "image/svg+xml; charset=utf-8",
                    svg.as_bytes(),
                )
                .await
            }
            Err(e) => {
                let msg = format!("could not render QR: {e}");
                respond(
                    stream,
                    Status::INTERNAL_ERROR,
                    "text/plain; charset=utf-8",
                    msg.as_bytes(),
                )
                .await
            }
        },
        // --- Getting the local CA onto the phone. ---
        //
        // Ungated, and for a stronger reason than "the certificate is public".
        // It is that **gating it is a deadlock**: the phone cannot present a
        // credential it has not been given, and this page is part of how it
        // becomes able to hold one at all. That the CA certificate is also not
        // a secret is a secondary comfort, not the reason — stating it the
        // other way round invites someone to gate this later once they think
        // of something secret to put here.
        //
        // See `crate::install` for what the page has to say and why the trust
        // check is two-stage.
        "/install" => match ctx.tls.as_ref() {
            Some(tls) => {
                let query = crate::install::split_query(full).1;
                let body = crate::install::page(&tls.install_page(query));
                respond(stream, Status::OK, "text/html; charset=utf-8", body.as_bytes()).await
            }
            None => {
                respond(
                    stream,
                    Status::NOT_FOUND,
                    "text/plain; charset=utf-8",
                    b"this bridge is not serving HTTPS, so there is no certificate to install",
                )
                .await
            }
        },
        // The CA's public certificate. The content type is what makes iOS
        // offer to install it rather than render it as text.
        "/ca.crt" => match ctx.tls.as_ref() {
            Some(tls) => {
                respond(
                    stream,
                    Status::OK,
                    crate::install::CA_CONTENT_TYPE,
                    tls.ca_cert_pem.as_bytes(),
                )
                .await
            }
            None => respond(stream, Status::NOT_FOUND, "text/plain; charset=utf-8", b"no certificate").await,
        },
        // Served on both listeners, ungated, and deliberately boring.
        //
        // Over TLS, *reaching this at all* proves the client validated our
        // certificate — which is only possible if the root is both installed
        // and trusted. That is the entire signal; the body is irrelevant.
        // Over plain HTTP it answers a different question, asked first: can the
        // phone reach this machine by name at all? Separating those two is what
        // lets the install page tell "your network is blocking mDNS" apart from
        // "you missed the trust step", which are the same symptom otherwise.
        //
        // The cross-origin header is required rather than lax: the page asking
        // is on the HTTP origin and the answer is on the HTTPS one, so the
        // check is cross-origin by construction. Nothing is disclosed by it.
        // The acceptance instrument. Meaningful only over TLS — asked over
        // plain HTTP it always answers "not secure", which is true and useless.
        // Ungated for the same reason as `/install`: it is a diagnostic a user
        // needs precisely when nothing else is working.
        "/secure-check" => {
            let body = crate::install::secure_check_page();
            respond(stream, 200, "text/html; charset=utf-8", body.as_bytes()).await
        }
        "/trustcheck" => {
            respond_with_headers(
                stream,
                Status::OK,
                "text/plain; charset=utf-8",
                "Access-Control-Allow-Origin: *\r\n",
                crate::install::TRUSTCHECK_BODY.as_bytes(),
            )
            .await
        }
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
        let body = placeholder_page(&ctx.pairing_url());
        return respond(
            stream,
            Status::OK,
            "text/html; charset=utf-8",
            body.as_bytes(),
        )
        .await;
    };

    let Some(rel) = safe_relative_path(path) else {
        return respond(
            stream,
            Status::FORBIDDEN,
            "text/plain; charset=utf-8",
            b"forbidden",
        )
        .await;
    };

    let mut file = root.join(&rel);
    if file.is_dir() {
        file = file.join("index.html");
    }

    match tokio::fs::read(&file).await {
        Ok(bytes) => respond(stream, Status::OK, mime_for(&file), &bytes).await,
        Err(_) => {
            // SPA fallback: unknown paths get index.html so client-side
            // routing works on a hard refresh.
            match tokio::fs::read(root.join("index.html")).await {
                Ok(bytes) => respond(stream, Status::OK, "text/html; charset=utf-8", &bytes).await,
                Err(_) => {
                    respond(
                        stream,
                        Status::NOT_FOUND,
                        "text/plain; charset=utf-8",
                        b"not found",
                    )
                    .await
                }
            }
        }
    }
}

/// The funscript library: an index, and the bytes.
///
/// All of the logic is in [`crate::library`] — this is the transport. Kept
/// small deliberately so the library feature is one arm in [`route`] and one
/// branch in [`ws_relay`], and does not compete for space in a file that TLS
/// work is also editing.
async fn serve_library<W>(stream: &mut W, path: &str, ctx: &Ctx) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let lib = ctx.library.as_deref();
    let Some(rest) = path.strip_prefix("/library/") else {
        return respond(
            stream,
            Status::NOT_FOUND,
            "text/plain; charset=utf-8",
            b"not found",
        )
        .await;
    };

    if rest == "index.json" {
        let body = library::index_json(lib);
        return respond(stream, Status::OK, "application/json; charset=utf-8", &body).await;
    }

    // No SPA fallback here. A missing script must read as a missing script;
    // serving `index.html` for it would hand the app an HTML document where it
    // expected a funscript, and the parse failure would name the wrong problem.
    match library::fetch(lib, rest).await {
        library::Fetched::Ok(bytes) => {
            respond(
                stream,
                Status::OK,
                "application/json; charset=utf-8",
                &bytes,
            )
            .await
        }
        library::Fetched::Rejected => {
            // Logged, because a rejection here is a traversal attempt or a
            // client encoding names wrongly, and both are worth seeing. The
            // name is escaped into the log via `{:?}` so a control character
            // cannot forge a log line.
            log_warn!("[library] refused a name: {rest:?}");
            respond(
                stream,
                Status::FORBIDDEN,
                "text/plain; charset=utf-8",
                b"forbidden",
            )
            .await
        }
        library::Fetched::NotFound => {
            // Logged as well as refused. A double-encoded traversal
            // (`%252e%252e%252f…`) decodes once to a literal `%2e%2e%2f…`,
            // which carries no separator and no `..` component — so it clears
            // the syntactic checks and dies at the index-membership one, as a
            // NotFound. Logging only rejections meant the most obvious probe to
            // try *after* `%2e%2e%2f` failed was the one that left no trace.
            log_debug!("[library] no such script: {rest:?}");
            respond(
                stream,
                Status::NOT_FOUND,
                "text/plain; charset=utf-8",
                b"not found",
            )
            .await
        }
        library::Fetched::TooLarge(size) => {
            log_warn!("[library] {rest:?} is {size} bytes; refusing to buffer it");
            respond(
                stream,
                Status::PAYLOAD_TOO_LARGE,
                "text/plain; charset=utf-8",
                b"script too large",
            )
            .await
        }
    }
}

/// Reject anything that could escape the static root: absolute paths, `..`,
/// Windows drive prefixes, UNC roots.
///
/// Shared with [`crate::library`] rather than reimplemented there, so the known
/// gap — it does not `canonicalize`, so a symlink inside the root escapes — has
/// one place to be fixed rather than two places to be fixed inconsistently.
/// `library::fetch` documents why that gap is acceptable for a directory the
/// user deliberately points at.
///
/// Note that this does **not** percent-decode, so `serve_static` cannot serve a
/// file whose name contains a space. `library::fetch` decodes first and then
/// calls this, which is the only safe order.
pub(crate) fn safe_relative_path(url_path: &str) -> Option<PathBuf> {
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

/// An HTTP status together with the reason phrase that belongs to it.
///
/// # Why this is a type and not a `u16`
///
/// It was a `u16`, and [`respond`] looked its phrase up in a `match` with a
/// `_ => "Internal Server Error"` fallback. So a status nobody had added an arm
/// for went out as `HTTP/1.1 413 Internal Server Error` — a size limit working
/// exactly as designed, announcing itself as a server fault, with the one line
/// whose job is to explain the status saying the opposite of it.
///
/// The first fix was a test listing every status and its phrase. That test was
/// **a second hand-typed copy of the match arms, not a derivation from the call
/// sites**: passing a bare `429` for rate limiting would have gone out as
/// `429 Internal Server Error` and the test would still have passed, because
/// 429 was never added to either list. The original defect, reproduced exactly,
/// under a green suite and a docstring claiming to prevent it.
///
/// So the phrase now travels *with* the status and there is no fallback to be
/// wrong. A status that does not exist here cannot be passed to `respond` at
/// all — it is a compile error, not a wrong string on the wire. Adding one
/// means adding a constant, which is the same amount of work as adding a match
/// arm and cannot be half-done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Status {
    code: u16,
    reason: &'static str,
}

impl Status {
    const fn new(code: u16, reason: &'static str) -> Self {
        Self { code, reason }
    }

    pub(crate) const OK: Self = Self::new(200, "OK");
    /// The exchange is a side effect reached by GET, and 303 says plainly
    /// "go and GET this other thing instead".
    pub(crate) const SEE_OTHER: Self = Self::new(303, "See Other");
    pub(crate) const BAD_REQUEST: Self = Self::new(400, "Bad Request");
    pub(crate) const UNAUTHORIZED: Self = Self::new(401, "Unauthorized");
    pub(crate) const FORBIDDEN: Self = Self::new(403, "Forbidden");
    pub(crate) const NOT_FOUND: Self = Self::new(404, "Not Found");
    pub(crate) const METHOD_NOT_ALLOWED: Self = Self::new(405, "Method Not Allowed");
    /// A pairing link this bridge does not recognise — usually a QR left over
    /// from an earlier run. 410 rather than 404 says the page is fine and
    /// *this link* is finished, which is what a stale QR actually is.
    pub(crate) const GONE: Self = Self::new(410, "Gone");
    pub(crate) const PAYLOAD_TOO_LARGE: Self = Self::new(413, "Payload Too Large");
    pub(crate) const HEADERS_TOO_LARGE: Self = Self::new(431, "Request Header Fields Too Large");
    pub(crate) const INTERNAL_ERROR: Self = Self::new(500, "Internal Server Error");
}

pub(crate) async fn respond<W>(
    stream: &mut W,
    status: Status,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    respond_with_headers(stream, status, content_type, "", body).await
}

/// As [`respond`], plus `extra` header lines (each `\r\n`-terminated).
///
/// One route needs a header the others must not have. Keeping it a per-route
/// opt-in is the point: the blanket version of this was the bug.
pub(crate) async fn respond_with_headers<W>(
    stream: &mut W,
    status: Status,
    content_type: &str,
    extra: &str,
    body: &[u8],
) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let Status {
        code: status,
        reason,
    } = status;
    // No blanket `Access-Control-Allow-Origin: *`. It was there to make a
    // browser on another origin able to read these responses, which is
    // precisely the thing that should not happen: it let any page in any tab
    // read `/healthz` and learn the media URL and LAN address.
    //
    // Exactly one route is legitimately cross-origin — `/trustcheck`, which is
    // *asked* from the plain-HTTP install page about the HTTPS listener, and so
    // is cross-origin by construction. It passes the header through `extra`
    // rather than reinstating it for everything.
    //
    // `Referrer-Policy` is explicit rather than left to the browser default.
    // The pairing URL carries the token in its query string, and a default of
    // `strict-origin-when-cross-origin` still sends the full URL on
    // *same-origin* requests — so every asset the install page loads would
    // carry the token in a `Referer`.
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Referrer-Policy: no-referrer\r\n\
         {extra}\
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

/// How often a snapshot is resent when nothing has changed.
///
/// Chosen to match the player's own observed cadence (~1006 ms), so a
/// connected client receives roughly one message per second whatever the
/// player is doing — including nothing.
pub const RELAY_KEEPALIVE: std::time::Duration = std::time::Duration::from_millis(1000);

/// Relay player state to one client, and accept commands back.
///
/// The message format is a stub, but an honest one: it carries exactly the
/// four things the spike promised — position, playing/paused, duration and
/// file identity — plus the link state, because "the bridge cannot see the
/// player" is a thing the phone has to render differently from "paused".
///
/// # Contract for consumers
///
/// A consumer deciding when to stop driving hardware needs to know exactly
/// what this stream guarantees. Stated as may / may not, because the
/// difference has already been load-bearing in one downstream design:
///
/// **You may assume:**
///
/// - **A snapshot arrives at least every [`RELAY_KEEPALIVE`]** while the
///   WebSocket is open, regardless of whether the player said anything. That
///   makes "the bridge is alive" independent of "the player is producing
///   traffic", which are different facts with different correct responses.
/// - **The snapshot is always current**, never a replay of an older one.
/// - **`link` distinguishes the three states** that matter: `connected` (the
///   bridge can see the player), `retrying`/`connecting` (it cannot), `idle`
///   (nobody asked it to). A player that has gone away is reported explicitly
///   rather than implied by silence.
/// - **`epoch` changes on every reconnect.** Position across a change is
///   unrelated to position before it.
/// - **Silence beyond the keepalive means the bridge or the socket is gone.**
///   Nothing else produces it.
///
/// **You may not assume:**
///
/// - **That one player packet produces one message.** State crosses a `watch`
///   channel, which keeps only the latest value. A consumer that is slow —
///   backgrounded tab, congested link, stalled render — collapses an
///   *unbounded* number of updates into a single delivery. This is measured,
///   not theoretical: 500 updates produce exactly one wake for a consumer that
///   is not polling. **There is no message-count-based deadline that can be
///   sized against this**, which is why the keepalive is a timer and not a
///   packet counter.
/// - **That message rate carries information about the player.** It does not.
///   Use `link`, `positionS` and `updatedAtMs`.
/// - **That `packets`, `attempts` or `stateSuspect` are stable API.** They are
///   diagnostics, and `packets` in particular resets on reconnect for reasons
///   that have nothing to do with continuity.
///
/// # A failure this socket cannot report
///
/// When the relay is reached over `wss://` and the certificate is not trusted,
/// the connection fails as close code **1006** with no further detail. There is
/// no interstitial and no error the page can inspect, because a WebSocket is a
/// subresource — the browser's certificate UI only exists for top-level
/// navigations. A trust problem and an unplugged router are byte-identical
/// here.
///
/// So **a consumer must not render 1006 as "the bridge is unreachable"**
/// without qualification. That string sends someone to check their Wi-Fi for a
/// problem that is in their certificate store. The distinguishing test is
/// whether the *same origin* loads in a top-level tab: if it does, trust and
/// network are both fine and the fault is elsewhere.
///
/// # Why this is written down at all
///
/// A *paused* player still produces snapshots at the keepalive cadence, so
/// "paused" and "dead" are distinguishable. That holds because `send_modify`
/// notifies unconditionally even when the closure changes nothing — asserted
/// in this module's tests rather than trusted to tokio's documentation, since
/// a safety decision rests on it.
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
    //
    // `library` is advertised whenever this build has the endpoints, configured
    // or not. Without it, a bridge too old to have a library and one that has
    // the feature but no folder set are indistinguishable — and probing to find
    // out is worse than asking, because on an old bridge `/library/index.json`
    // falls through to the SPA handler and returns **200 with `index.html`**.
    // The client then gets a JSON parse error, which names the wrong problem.
    let hello = serde_json::json!({
        "type": "hello",
        "bridge": env!("CARGO_PKG_NAME"),
        "version": env!("CARGO_PKG_VERSION"),
        "carries": ["position", "playing", "duration", "media", "library"],
        "accepts": ["seek", "play", "pause"],
        "note": "spike build — message shape is not stable",
    });
    if tx.send(Message::Text(hello.to_string())).await.is_err() {
        return;
    }

    // Wakes when the library directory's contents change. `None` when no
    // library is configured, which is a normal state — see [`crate::library`].
    let mut library_rx = ctx.library.as_ref().map(|l| l.subscribe());

    // One `library` message up front, **whether or not a library is
    // configured**, so "fetch the index whenever a `library` message arrives"
    // is the client's only rule. Without it the client needs a second rule for
    // startup, and two rules that must agree is how a phone ends up with a
    // listing it never refreshes. The unconfigured case costs one fetch that
    // comes back `configured: false`.
    let initial = library_rx
        .as_mut()
        .map(|rx| rx.borrow_and_update().clone())
        .unwrap_or_default();
    if tx
        .send(Message::Text(library::change_message(&initial)))
        .await
        .is_err()
    {
        return;
    }

    // Send current state immediately; a reconnecting phone must not wait for
    // the next change to learn where playback is.
    let initial = snapshots.borrow_and_update().clone();
    if send_snapshot(&mut tx, &initial).await.is_err() {
        return;
    }

    // Resends the current snapshot when the player has been quiet. Reset on
    // every change-driven send, so a busy link carries no extra traffic and a
    // silent one still proves the bridge is alive.
    let mut keepalive = tokio::time::interval(RELAY_KEEPALIVE);
    keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    keepalive.tick().await; // the immediate first tick

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
                keepalive.reset();
            }
            _ = keepalive.tick() => {
                // Nothing changed within the window. Send anyway: the consumer
                // is entitled to conclude "the bridge is gone" from silence,
                // so silence must mean only that.
                //
                // Deliberately the same message rather than a distinct ping.
                // A separate type would invite consumers to treat the two
                // differently, and there is no difference worth acting on —
                // the snapshot is current either way.
                let snap = snapshots.borrow().clone();
                if send_snapshot(&mut tx, &snap).await.is_err() {
                    break;
                }
            }
            // A file appeared, vanished or was renamed in the library
            // directory. The message says only "ask again" — contents travel
            // over `/library/index.json`, which is a request/response with its
            // own freshness stamp, rather than over a `watch` this doc comment
            // has already established can collapse unboundedly.
            changed = async {
                match library_rx.as_mut() {
                    Some(rx) => rx.changed().await.is_ok(),
                    // No library configured: this branch must never be ready,
                    // or the select would spin.
                    None => std::future::pending().await,
                }
            } => {
                if !changed {
                    break; // bridge shutting down
                }
                let index = library_rx
                    .as_mut()
                    .map(|rx| rx.borrow_and_update().clone())
                    .unwrap_or_default();
                if tx.send(Message::Text(library::change_message(&index))).await.is_err() {
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

    /// **Q1: does a paused player still produce pushes?**
    ///
    /// A paused DeoVR keeps sending `currentTime` unchanged at its normal
    /// cadence, so the snapshot's *playback* fields are identical each time.
    /// If `watch` suppressed a no-op modify, a paused player would produce
    /// zero pushes and a consumer's "paused" state would be unreachable —
    /// it would see silence and stop, on a working setup.
    ///
    /// It does not. `send_modify` notifies unconditionally, unlike
    /// `send_if_modified`. Asserted rather than trusted to the documentation,
    /// because a downstream safety decision rests on it.
    #[tokio::test]
    async fn an_identical_snapshot_still_wakes_the_relay() {
        let (tx, mut rx) = watch::channel(PlayerSnapshot::new("x".into()));
        rx.borrow_and_update();

        // A closure that changes precisely nothing.
        tx.send_modify(|_s| {});

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(200), rx.changed())
                .await
                .is_ok(),
            "a no-op send_modify must still wake the relay, or a paused player \
             is indistinguishable from a dead bridge"
        );
    }

    /// And the real path: `apply` on a repeated identical packet.
    ///
    /// It changes `packets` and `updated_at_ms` even when nothing else moves,
    /// so the value genuinely differs — but the notification does not depend
    /// on that, which is the point of the test above.
    #[tokio::test]
    async fn a_repeated_packet_from_a_paused_player_still_pushes() {
        let (tx, mut rx) = watch::channel(PlayerSnapshot::new("x".into()));
        rx.borrow_and_update();

        let packet: crate::state::PlayerPacket =
            serde_json::from_str(r#"{"path":"a.mp4","currentTime":42.0,"playerState":1}"#).unwrap();

        for tick in 1..=3u64 {
            tx.send_modify(|s| s.apply(&packet, 1000 + tick * 1000));
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(200), rx.changed())
                    .await
                    .is_ok(),
                "push {tick} from a paused player was suppressed"
            );
            assert_eq!(rx.borrow().position_s, Some(42.0));
        }
        assert_eq!(rx.borrow().packets, 3, "every packet must be counted");
    }

    /// **Q2: how far can pushes collapse when the consumer is slow?**
    ///
    /// All the way. `watch` keeps one slot, so a consumer that is not polling
    /// observes a single `changed()` no matter how many updates landed while
    /// it was away. There is no bound to quote and no deadline that can be
    /// sized against it — which is the answer, and the reason the relay sends
    /// a keepalive on a timer rather than relying on player traffic.
    #[tokio::test]
    async fn an_unbounded_number_of_updates_collapse_into_one_wake() {
        let (tx, mut rx) = watch::channel(PlayerSnapshot::new("x".into()));
        rx.borrow_and_update();

        let packet: crate::state::PlayerPacket =
            serde_json::from_str(r#"{"currentTime":1.0}"#).unwrap();
        for tick in 0..500u64 {
            tx.send_modify(|s| s.apply(&packet, tick));
        }

        // One wake, for five hundred updates.
        assert!(rx.changed().await.is_ok());
        assert_eq!(rx.borrow_and_update().packets, 500, "state is the latest");

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), rx.changed())
                .await
                .is_err(),
            "there is no second wake — 500 updates produced exactly one"
        );
    }

    fn test_ctx() -> Ctx {
        let (_snap_tx, snapshot_rx) = watch::channel(PlayerSnapshot::new("127.0.0.1:23554".into()));
        let (cmd_tx, _cmd_rx) = mpsc::channel(4);
        Ctx {
            snapshot_rx,
            cmd_tx,
            static_dir: None,
            library: None,
            pairing_base: "http://192.168.0.9:8787".into(),
            token: std::sync::RwLock::new(Token::generate()),
            allowed_hosts: vec!["192.168.0.9:8787".into()],
            on_token_rotated: None,
        }
    }

    /// The pairing URL must follow the token, not a copy of it.
    ///
    /// Regression: `pairing_url` used to be a `String` fixed at startup, so
    /// after a rotation `/pair` and `/qr.svg` would have gone on serving a QR
    /// encoding the token that had just been revoked. Nothing called rotation
    /// yet, so it had never fired — but a revoke button that leaves the QR
    /// advertising the revoked credential is worse than no revoke button.
    #[test]
    fn the_pairing_url_follows_a_rotation() {
        let ctx = test_ctx();
        let before = ctx.pairing_url();
        assert!(before.contains(ctx.token().as_str()));

        let fresh = ctx.rotate_token();

        let after = ctx.pairing_url();
        assert_ne!(before, after, "the URL must change with the token");
        assert!(after.contains(fresh.as_str()));
        assert!(
            !after.contains(before.rsplit("t=").next().unwrap()),
            "the revoked token must not survive in the URL"
        );
    }

    /// Rotation notifies its owner, which is how the desktop app persists the
    /// new token. Without it, a revoke would silently revert on restart.
    #[test]
    fn rotation_notifies_the_owner() {
        let seen = Arc::new(std::sync::Mutex::new(None::<String>));
        let sink = Arc::clone(&seen);
        let mut ctx = test_ctx();
        ctx.on_token_rotated = Some(Box::new(move |t: &Token| {
            *sink.lock().unwrap() = Some(t.as_str().to_string());
        }));

        let fresh = ctx.rotate_token();
        assert_eq!(seen.lock().unwrap().as_deref(), Some(fresh.as_str()));
    }

    /// The old token stops authorising the moment it is revoked.
    #[test]
    fn a_revoked_token_no_longer_authorises() {
        let ctx = test_ctx();
        let old = ctx.token();
        let target = format!("/healthz?t={old}");
        assert!(authorised(&target, None, &ctx));

        ctx.rotate_token();
        assert!(
            !authorised(&target, None, &ctx),
            "a revoked token must be refused"
        );
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

    /// The status line is assembled from the [`Status`] it was handed.
    ///
    /// Deliberately **not** a list of every status and its phrase. That is what
    /// this test used to be, and it was a second hand-typed copy of the same
    /// table it was checking — so a status added at a call site but not to
    /// either list went out as `Internal Server Error` and the test still
    /// passed. The enumeration problem is now solved by the type: a status that
    /// is not a `Status` constant cannot reach `respond` at all, and adding one
    /// carries its phrase with it.
    ///
    /// What is left to check is the wire format, which is what this does.
    #[tokio::test]
    async fn the_status_line_carries_the_code_and_its_phrase() {
        let mut sink = Vec::new();
        respond(&mut sink, Status::PAYLOAD_TOO_LARGE, "text/plain", b"x")
            .await
            .unwrap();
        let head = String::from_utf8_lossy(&sink);
        assert!(
            head.starts_with("HTTP/1.1 413 Payload Too Large\r\n"),
            "got: {}",
            head.lines().next().unwrap_or_default()
        );
        assert!(head.contains("Content-Length: 1\r\n"));
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
