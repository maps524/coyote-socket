//! Relaying media bytes from the media server to the phone, with `Range`
//! handled properly.
//!
//! ## Why the bridge is in the playback path at all
//!
//! The app is on HTTPS, because Web Bluetooth requires a secure context. An
//! HTTPS page cannot load an `http://` video — mixed content is blocked
//! outright, with no override. The media server speaks plain HTTP and will not
//! be getting a certificate the phone trusts. So the only remaining place for
//! the transition is a process the phone *already* trusts, which is this one.
//!
//! That is a cost, and it should be stated rather than discovered:
//!
//! - **Co-located (the expected deployment).** Bridge and media server on the
//!   same machine, so the upstream fetch is a loopback socket. The bytes cross
//!   the network once, from the bridge to the phone. The extra work is a memcpy
//!   through a 64 KiB buffer and TLS encryption that the phone required anyway.
//! - **Separate machines.** The bytes cross the network **twice** — server to
//!   bridge, bridge to phone — and if both hops share one Wi-Fi radio, the
//!   available throughput for the stream is roughly halved. For a 25 Mb/s VR
//!   file on a link that can do 100 Mb/s this is invisible; on a congested
//!   2.4 GHz network it is the difference between playing and stalling. There
//!   is no way to avoid it that keeps the page on HTTPS.
//!
//! ## The failure mode this module exists to avoid
//!
//! [`crate::http::respond`] writes a whole body from a `&[u8]`. Using it here
//! would be the shortest path to something that appears to work — and it would
//! allocate the entire file. On a 40 GB VR video that is not a slow response,
//! it is a dead process, and it will have passed every test done on a short
//! clip. Nothing in this module ever holds more than [`COPY_BUFFER`] bytes of
//! media.
//!
//! ## Ranges, which are the acceptance condition
//!
//! A proxy that ignores `Range` plays from the beginning and fails only when
//! someone seeks — and the blame lands on the video file. The rules followed
//! here:
//!
//! - `Range` and `If-Range` are forwarded upstream **verbatim**. The upstream
//!   server is the one that knows the file's length and can answer correctly.
//! - The upstream **status is preserved**. A `206` stays a `206`; rewriting it
//!   to `200` is the single most common way a proxy breaks seeking, because the
//!   browser then believes it received the whole file starting at byte zero.
//! - `Content-Range`, `Accept-Ranges`, `Content-Length`, `Content-Type`,
//!   `ETag` and `Last-Modified` are passed through. `Content-Range` is what
//!   carries the total size, and without it a browser cannot build a scrub bar.
//! - A `416` is passed through as a `416`, because it means the client asked
//!   past the end and needs to know that.
//! - `bytes=N-` — the open-ended form, which is what every browser actually
//!   sends on a seek — is handled by the same path, since it is forwarded as
//!   written.
//!
//! **And one case where forwarding is not enough.** Some servers ignore `Range`
//! and answer `200` with the whole file. Relaying that gives a player that
//! jumps back to the start on every seek. When the request was a simple
//! `bytes=N-` and `N` is within [`MAX_SKIP`], this module reads and discards
//! the first `N` bytes upstream and synthesises the `206` itself. Beyond that
//! bound it relays the `200` and logs which happened, because silently reading
//! four gigabytes to satisfy a scrub is worse than the scrub not working.

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::httpc::{self, Framing, Head, Url};
use crate::{log_debug, log_info, log_warn};

/// The copy buffer. The only media-sized allocation in the process.
const COPY_BUFFER: usize = 64 * 1024;

/// How long a single upstream read may stall before the transfer is abandoned.
///
/// There is deliberately no *total* deadline — a two-hour film is a two-hour
/// transfer — but a server that goes silent mid-file must not pin a task
/// indefinitely, and TCP's own timeout is measured in minutes.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// The furthest into a file this module will seek by reading and discarding,
/// when the upstream server ignores `Range`.
///
/// 64 MiB is roughly twenty seconds of a high-bitrate VR file: enough to make
/// small scrubs work against a server that does not advertise byte ranges,
/// small enough that it can never become a multi-gigabyte read nobody asked
/// for.
const MAX_SKIP: u64 = 64 * 1024 * 1024;

/// A parsed `Range` header, restricted to the forms that matter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeSpec {
    /// `bytes=N-M`, inclusive.
    FromTo(u64, u64),
    /// `bytes=N-`. The open-ended form; what a browser sends on a seek.
    From(u64),
    /// `bytes=-S`: the last `S` bytes.
    Suffix(u64),
}

/// Parse a `Range` header value.
///
/// Returns `None` for anything not understood — including multi-range, which
/// no media element sends and which a proxy answering `multipart/byteranges`
/// would have to synthesise. RFC 9110 permits a server to ignore a `Range` it
/// does not support, so `None` here means "forward without it", not "reject".
pub fn parse_range(value: &str) -> Option<RangeSpec> {
    let spec = value.trim().strip_prefix("bytes=")?.trim();
    if spec.contains(',') {
        return None;
    }
    let (start, end) = spec.split_once('-')?;
    let (start, end) = (start.trim(), end.trim());
    match (start.is_empty(), end.is_empty()) {
        // `bytes=-500`
        (true, false) => {
            let n: u64 = end.parse().ok()?;
            (n > 0).then_some(RangeSpec::Suffix(n))
        }
        // `bytes=500-`
        (false, true) => Some(RangeSpec::From(start.parse().ok()?)),
        // `bytes=0-499`
        (false, false) => {
            let (s, e) = (start.parse().ok()?, end.parse().ok()?);
            (s <= e).then_some(RangeSpec::FromTo(s, e))
        }
        (true, true) => None,
    }
}

/// What happened, for the log and for the caller's diagnosis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Upstream answered and bytes were relayed. Carries the status sent
    /// downstream and how many body bytes moved.
    Relayed { status: u16, bytes: u64 },
    /// Upstream could not be reached or answered nonsense. Nothing was written
    /// downstream beyond the error response.
    Upstream(String),
}

/// Proxy one media request.
///
/// `method` is `GET` or `HEAD`. `range` and `if_range` are the client's header
/// values, forwarded as given. `allowed_authority` is the `host:port` the
/// upstream URL must belong to — see [`crate::dlna`] for why that check exists
/// and why it is the caller's to make.
pub async fn proxy<W>(
    out: &mut W,
    method: &str,
    upstream: &Url,
    range: Option<&str>,
    if_range: Option<&str>,
) -> Outcome
where
    W: AsyncWrite + Unpin,
{
    let mut headers: Vec<(&str, &str)> = Vec::new();
    if let Some(r) = range {
        headers.push(("Range", r));
    }
    if let Some(ir) = if_range {
        headers.push(("If-Range", ir));
    }

    let opened = match httpc::open(method, upstream, &headers).await {
        Ok(o) => o,
        Err(e) => {
            // §0b: name the component that actually failed. "502" alone would
            // land the blame on the bridge, which merely relayed a refusal.
            log_warn!("[media] upstream {upstream} failed: {e}");
            let msg = format!("the media server at {} did not answer: {e}", upstream.authority());
            let _ = write_head(out, 502, &[("Content-Type", "text/plain; charset=utf-8"),
                ("Content-Length", &msg.len().to_string())]).await;
            let _ = out.write_all(msg.as_bytes()).await;
            return Outcome::Upstream(msg);
        }
    };

    let httpc::Open {
        head,
        framing,
        mut body,
    } = opened;

    // The upstream ignored a simple `bytes=N-`. Synthesise the 206 by
    // discarding, when the distance is bounded. See the module docs.
    if head.status == 200 {
        if let Some(RangeSpec::From(start)) = range.and_then(parse_range) {
            if start > 0 {
                let total = head.content_length();
                return match (start <= MAX_SKIP, total) {
                    (true, Some(total)) if start < total => {
                        log_info!(
                            "[media] {} ignored Range; skipping {start} bytes to synthesise a 206",
                            upstream.authority()
                        );
                        relay_skipped(out, method, &head, &mut body, start, total).await
                    }
                    _ => {
                        log_warn!(
                            "[media] {} ignored Range: bytes={start}- and the gap is {}; relaying \
                             200, so the player will restart from the beginning rather than seek",
                            upstream.authority(),
                            if total.is_some() { "too large to skip" } else { "unknowable" }
                        );
                        relay(out, method, &head, framing, &mut body, 200).await
                    }
                };
            }
        }
    }

    relay(out, method, &head, framing, &mut body, head.status).await
}

/// Forward the upstream response as-is.
async fn relay<W, R>(
    out: &mut W,
    method: &str,
    head: &Head,
    framing: Framing,
    body: &mut R,
    status: u16,
) -> Outcome
where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    let mut headers: Vec<(String, String)> = Vec::new();
    // Everything a media element needs, and nothing else. An allowlist rather
    // than a denylist: an upstream `Set-Cookie`, `Access-Control-Allow-Origin`
    // or `Content-Encoding` reaching the phone would be someone else's header
    // arriving with this origin's authority.
    for name in [
        "content-type",
        "content-length",
        "content-range",
        "accept-ranges",
        "etag",
        "last-modified",
        "content-disposition",
    ] {
        if let Some(v) = head.get(name) {
            headers.push((canonical(name), v.to_string()));
        }
    }
    if head.get("accept-ranges").is_none() && matches!(framing, Framing::Length(_)) {
        // The upstream knows its own length, so ranges are at least possible.
        // Saying nothing here makes some players refuse to offer a scrub bar.
        headers.push(("Accept-Ranges".into(), "bytes".into()));
    }
    if !matches!(framing, Framing::Length(_)) {
        // No length: the body ends when the connection does. Strip any
        // `Content-Length` the upstream contradicted itself with, and make sure
        // nothing downstream is told a size it cannot rely on.
        headers.retain(|(k, _)| k != "Content-Length");
        log_debug!("[media] upstream body is {framing:?}; this response is not seekable");
    }
    headers.push(("Cache-Control".into(), "no-store".into()));
    headers.push(("X-Content-Type-Options".into(), "nosniff".into()));
    headers.push(("Connection".into(), "close".into()));

    let pairs: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    if write_head(out, status, &pairs).await.is_err() {
        return Outcome::Relayed { status, bytes: 0 };
    }

    if method == "HEAD" {
        // A HEAD carries the headers and no body. Getting this wrong is not
        // cosmetic: a player that issues HEAD first to learn the length would
        // read the body as the start of its next response.
        let _ = out.flush().await;
        return Outcome::Relayed { status, bytes: 0 };
    }

    let limit = match framing {
        Framing::Length(n) => Some(n),
        _ => None,
    };
    let moved = copy_body(out, body, limit).await;
    Outcome::Relayed {
        status,
        bytes: moved,
    }
}

/// Forward a `200` as a `206`, by discarding `start` bytes first.
async fn relay_skipped<W, R>(
    out: &mut W,
    method: &str,
    head: &Head,
    body: &mut R,
    start: u64,
    total: u64,
) -> Outcome
where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    let remaining = total - start;
    let content_range = format!("bytes {start}-{}/{total}", total - 1);
    let length = remaining.to_string();
    let ctype = head.get("content-type").unwrap_or("application/octet-stream");

    let mut headers: Vec<(&str, &str)> = vec![
        ("Content-Type", ctype),
        ("Content-Length", &length),
        ("Content-Range", &content_range),
        ("Accept-Ranges", "bytes"),
        ("Cache-Control", "no-store"),
        ("X-Content-Type-Options", "nosniff"),
        ("Connection", "close"),
    ];
    if let Some(v) = head.get("last-modified") {
        headers.push(("Last-Modified", v));
    }

    // The head goes out only after the skip has succeeded — otherwise a failure
    // partway through discarding would leave a 206 promising bytes that are
    // never coming, which is worse than a clean 502.
    if let Err(e) = discard(body, start).await {
        log_warn!("[media] could not skip {start} bytes upstream: {e}");
        let msg = format!("the media server stopped while seeking to {start}: {e}");
        let _ = write_head(
            out,
            502,
            &[
                ("Content-Type", "text/plain; charset=utf-8"),
                ("Content-Length", &msg.len().to_string()),
            ],
        )
        .await;
        let _ = out.write_all(msg.as_bytes()).await;
        return Outcome::Upstream(msg);
    }

    if write_head(out, 206, &headers).await.is_err() {
        return Outcome::Relayed {
            status: 206,
            bytes: 0,
        };
    }
    if method == "HEAD" {
        let _ = out.flush().await;
        return Outcome::Relayed {
            status: 206,
            bytes: 0,
        };
    }
    let moved = copy_body(out, body, Some(remaining)).await;
    Outcome::Relayed {
        status: 206,
        bytes: moved,
    }
}

/// Read and throw away `n` bytes.
async fn discard<R: AsyncRead + Unpin>(r: &mut R, n: u64) -> std::io::Result<()> {
    let mut buf = vec![0u8; COPY_BUFFER];
    let mut left = n;
    while left > 0 {
        let want = left.min(COPY_BUFFER as u64) as usize;
        let read = tokio::time::timeout(IDLE_TIMEOUT, r.read(&mut buf[..want]))
            .await
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::TimedOut, "upstream stalled while skipping")
            })??;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "upstream ended before the seek target",
            ));
        }
        left -= read as u64;
    }
    Ok(())
}

/// Stream the body downstream. Returns how many bytes moved.
///
/// `limit` is `Some` when the upstream declared a length, in which case exactly
/// that many bytes are forwarded — reading further would consume whatever
/// follows on a reused connection, and stopping short would truncate the file.
async fn copy_body<W, R>(out: &mut W, body: &mut R, limit: Option<u64>) -> u64
where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    let mut buf = vec![0u8; COPY_BUFFER];
    let mut moved = 0u64;
    loop {
        let want = match limit {
            Some(total) if moved >= total => break,
            Some(total) => ((total - moved).min(COPY_BUFFER as u64)) as usize,
            None => COPY_BUFFER,
        };
        let read = match tokio::time::timeout(IDLE_TIMEOUT, body.read(&mut buf[..want])).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => n,
            Ok(Err(e)) => {
                log_debug!("[media] upstream read ended: {e}");
                break;
            }
            Err(_) => {
                log_warn!("[media] upstream stalled for {IDLE_TIMEOUT:?}; dropping the transfer");
                break;
            }
        };
        if out.write_all(&buf[..read]).await.is_err() {
            // The phone hung up — every seek does this, since the browser
            // abandons the in-flight request and opens a new one. Debug, not
            // warn: at warn level a normal scrub would fill the log.
            log_debug!("[media] client went away after {moved} bytes");
            break;
        }
        moved += read as u64;
    }
    let _ = out.flush().await;
    moved
}

/// Write a response head with an arbitrary status and header set.
///
/// Separate from [`crate::http::respond`] deliberately: that function's
/// contract is "here is a complete body in memory", which is exactly what must
/// not happen to a media file. Its status table also stops at 431 and it has no
/// notion of `206` or `Content-Range`.
async fn write_head<W>(out: &mut W, status: u16, headers: &[(&str, &str)]) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let reason = match status {
        200 => "OK",
        206 => "Partial Content",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        416 => "Range Not Satisfiable",
        502 => "Bad Gateway",
        504 => "Gateway Timeout",
        _ => "Internal Server Error",
    };
    let mut text = format!("HTTP/1.1 {status} {reason}\r\n");
    for (k, v) in headers {
        if v.contains('\r') || v.contains('\n') {
            continue;
        }
        text.push_str(&format!("{k}: {v}\r\n"));
    }
    text.push_str("\r\n");
    out.write_all(text.as_bytes()).await
}

/// `content-range` -> `Content-Range`. Cosmetic, but a header the phone's
/// developer tools will show.
fn canonical(lower: &str) -> String {
    lower
        .split('-')
        .map(|part| {
            let mut c = part.chars();
            match c.next() {
                Some(f) => f.to_ascii_uppercase().to_string() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    //! Every test drives an in-memory duplex. Nothing here binds a port or
    //! dials an address — see [`crate::ssdp`]'s test module for why that is
    //! written down in this crate rather than assumed.

    use super::*;

    #[test]
    fn parses_the_forms_a_browser_sends() {
        // The open-ended form. This is what a seek looks like, and it is the
        // one a naive parser drops because there is nothing after the dash.
        assert_eq!(parse_range("bytes=1000-"), Some(RangeSpec::From(1000)));
        assert_eq!(parse_range("bytes=0-"), Some(RangeSpec::From(0)));
        assert_eq!(parse_range("bytes=0-1023"), Some(RangeSpec::FromTo(0, 1023)));
        assert_eq!(parse_range("bytes=-500"), Some(RangeSpec::Suffix(500)));
        // Whitespace around the numbers is tolerated; whitespace around the
        // `=` is not, because RFC 9110 does not allow it there and accepting it
        // would mean accepting a header no client sends.
        assert_eq!(parse_range(" bytes= 5 - 9 "), Some(RangeSpec::FromTo(5, 9)));
        assert_eq!(parse_range("bytes = 5-9"), None);
    }

    #[test]
    fn refuses_what_it_cannot_answer() {
        // Multi-range would need a multipart/byteranges body. No media element
        // sends one; forwarding without the header is the specified fallback.
        assert_eq!(parse_range("bytes=0-99,200-299"), None);
        assert_eq!(parse_range("bytes=-"), None);
        assert_eq!(parse_range("bytes=9-5"), None, "end before start");
        assert_eq!(parse_range("items=0-1"), None, "only bytes are ranges");
        assert_eq!(parse_range("bytes=abc-"), None);
        assert_eq!(parse_range("bytes=-0"), None, "a zero-length suffix is meaningless");
    }

    fn head(raw: &str) -> Head {
        let mut headers = Vec::new();
        let mut lines = raw.split("\r\n");
        let status: u16 = lines
            .next()
            .unwrap()
            .split(' ')
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        for line in lines {
            if let Some((k, v)) = line.split_once(':') {
                headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
            }
        }
        Head { status, headers }
    }

    async fn relay_to_string(
        method: &str,
        upstream_head: &str,
        upstream_body: &[u8],
        status_override: Option<u16>,
    ) -> (String, Vec<u8>) {
        let h = head(upstream_head);
        let framing = match h.content_length() {
            Some(n) => Framing::Length(n),
            None => Framing::ToEof,
        };
        let mut body = std::io::Cursor::new(upstream_body.to_vec());
        let mut out = Vec::new();
        let status = status_override.unwrap_or(h.status);
        relay(&mut out, method, &h, framing, &mut body, status).await;
        let split = out
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("a head must be written");
        (
            String::from_utf8_lossy(&out[..split]).into_owned(),
            out[split + 4..].to_vec(),
        )
    }

    /// **The defect that ships.** A 206 relayed as a 200 plays from the start
    /// and fails only on a seek, and the video gets the blame.
    #[tokio::test]
    async fn a_206_stays_a_206_and_keeps_its_content_range() {
        let (head_text, body) = relay_to_string(
            "GET",
            "HTTP/1.1 206 Partial Content\r\nContent-Type: video/mp4\r\n\
             Content-Length: 5\r\nContent-Range: bytes 1000-1004/9999\r\nAccept-Ranges: bytes",
            b"ABCDE",
            None,
        )
        .await;
        assert!(head_text.starts_with("HTTP/1.1 206 Partial Content"), "{head_text}");
        assert!(head_text.contains("Content-Range: bytes 1000-1004/9999"), "{head_text}");
        assert!(head_text.contains("Accept-Ranges: bytes"), "{head_text}");
        assert!(head_text.contains("Content-Length: 5"), "{head_text}");
        assert_eq!(body, b"ABCDE");
    }

    /// A `416` must survive too: it is how a client learns it asked past the
    /// end, and swallowing it produces a player that retries forever.
    #[tokio::test]
    async fn a_416_is_passed_through() {
        let (head_text, _) = relay_to_string(
            "GET",
            "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */9999\r\nContent-Length: 0",
            b"",
            None,
        )
        .await;
        assert!(head_text.starts_with("HTTP/1.1 416"), "{head_text}");
        assert!(head_text.contains("Content-Range: bytes */9999"));
    }

    /// A HEAD carries no body. A player that HEADs first to learn the length
    /// would otherwise read the file as its next response.
    #[tokio::test]
    async fn a_head_request_gets_headers_and_no_body() {
        let (head_text, body) = relay_to_string(
            "HEAD",
            "HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nContent-Length: 9999",
            b"this should not be sent",
            None,
        )
        .await;
        assert!(head_text.contains("Content-Length: 9999"));
        assert!(body.is_empty(), "a HEAD response must have no body");
    }

    /// Exactly `Content-Length` bytes, no more. Over-reading takes bytes that
    /// belong to whatever follows; under-reading truncates the file.
    #[tokio::test]
    async fn the_body_is_cut_at_the_declared_length() {
        let (_, body) = relay_to_string(
            "GET",
            "HTTP/1.1 200 OK\r\nContent-Length: 4",
            b"ABCDEFGHIJ",
            None,
        )
        .await;
        assert_eq!(body, b"ABCD");
    }

    /// Someone else's headers must not arrive wearing this origin's authority.
    #[tokio::test]
    async fn upstream_headers_are_allowlisted() {
        let (head_text, _) = relay_to_string(
            "GET",
            "HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nContent-Length: 1\r\n\
             Set-Cookie: session=abc\r\nAccess-Control-Allow-Origin: *\r\nServer: UMS",
            b"A",
            None,
        )
        .await;
        assert!(!head_text.contains("Set-Cookie"), "{head_text}");
        assert!(!head_text.contains("Access-Control-Allow-Origin"), "{head_text}");
        assert!(!head_text.to_ascii_lowercase().contains("server: ums"), "{head_text}");
        assert!(head_text.contains("Content-Type: video/mp4"));
    }

    /// An unlengthed body must not carry a `Content-Length`, or the browser
    /// waits forever for bytes that are not coming.
    #[tokio::test]
    async fn an_unlengthed_body_carries_no_content_length() {
        let (head_text, body) = relay_to_string(
            "GET",
            "HTTP/1.1 200 OK\r\nContent-Type: video/mp4",
            b"streamed",
            None,
        )
        .await;
        assert!(!head_text.contains("Content-Length"), "{head_text}");
        assert!(head_text.contains("Connection: close"));
        assert_eq!(body, b"streamed");
    }

    /// A server that declares a length but no `Accept-Ranges` still gets one,
    /// because some players will not show a scrub bar without it.
    #[tokio::test]
    async fn a_lengthed_response_advertises_range_support() {
        let (head_text, _) = relay_to_string(
            "GET",
            "HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nContent-Length: 3",
            b"abc",
            None,
        )
        .await;
        assert!(head_text.contains("Accept-Ranges: bytes"), "{head_text}");
    }

    /// The synthesised 206, for a server that ignored the `Range` it was sent.
    #[tokio::test]
    async fn a_range_ignoring_upstream_still_produces_a_correct_206() {
        let h = head("HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nContent-Length: 10");
        let mut body = std::io::Cursor::new(b"0123456789".to_vec());
        let mut out = Vec::new();
        let outcome = relay_skipped(&mut out, "GET", &h, &mut body, 4, 10).await;

        let split = out.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head_text = String::from_utf8_lossy(&out[..split]).into_owned();
        assert!(head_text.starts_with("HTTP/1.1 206"), "{head_text}");
        assert!(head_text.contains("Content-Range: bytes 4-9/10"), "{head_text}");
        assert!(head_text.contains("Content-Length: 6"), "{head_text}");
        assert_eq!(&out[split + 4..], b"456789");
        assert_eq!(outcome, Outcome::Relayed { status: 206, bytes: 6 });
    }

    /// If the skip cannot complete, nothing has promised anything yet — so the
    /// client gets a 502 rather than a 206 whose body never arrives.
    #[tokio::test]
    async fn a_failed_skip_answers_502_not_a_short_206() {
        let h = head("HTTP/1.1 200 OK\r\nContent-Length: 100");
        let mut body = std::io::Cursor::new(b"short".to_vec());
        let mut out = Vec::new();
        let outcome = relay_skipped(&mut out, "GET", &h, &mut body, 50, 100).await;

        let text = String::from_utf8_lossy(&out);
        assert!(text.starts_with("HTTP/1.1 502"), "{text}");
        assert!(matches!(outcome, Outcome::Upstream(_)));
        // §0b: the message names the media server, not this bridge.
        assert!(text.contains("media server"), "{text}");
    }

    #[test]
    fn header_names_are_canonicalised() {
        assert_eq!(canonical("content-range"), "Content-Range");
        assert_eq!(canonical("etag"), "Etag");
    }

    #[tokio::test]
    async fn a_header_value_with_crlf_cannot_inject_a_header() {
        let mut out = Vec::new();
        write_head(&mut out, 200, &[("X-Test", "a\r\nX-Evil: 1")])
            .await
            .unwrap();
        assert!(!String::from_utf8_lossy(&out).contains("X-Evil"));
    }
}
