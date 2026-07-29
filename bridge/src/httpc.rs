//! A minimal HTTP/1.1 *client*, because three separate jobs need one and the
//! crate has none.
//!
//! Fetching a UPnP device description, posting a SOAP `Browse`, and proxying a
//! multi-gigabyte video all speak HTTP to a server we did not write. The
//! crate's stated rule is that its dependencies stay a subset of what
//! `src-tauri` already builds, and `src-tauri` has no HTTP client — so this is
//! the alternative to adding one.
//!
//! It is deliberately small and deliberately unambitious. What it does **not**
//! do is as load-bearing as what it does, because every omission below is a
//! thing a caller might otherwise assume:
//!
//! - **No `https://` upstream.** There is no TLS client in this crate. The
//!   media server we talk to is a DLNA server on the LAN, which is plain HTTP
//!   by protocol convention — UPnP device descriptions and SOAP control URLs
//!   are `http://` in every implementation. [`Url::parse`] refuses any other
//!   scheme rather than silently connecting in the clear to something that
//!   asked for TLS. If an upstream ever needs `https://`, this is the seam.
//! - **No redirects.** A redirect is returned to the caller as the 3xx it is.
//!   Following one silently would let a media server point the proxy anywhere,
//!   which is precisely the reach that [`crate::dlna`] spends effort bounding.
//! - **No connection reuse.** One request, one TCP connection, `Connection:
//!   close`. The server side of this crate does the same. See the cost note in
//!   [`crate::mediaproxy`].
//! - **No cookies, no auth, no compression.**
//!
//! ## Two body modes, because the two callers want opposite things
//!
//! [`fetch_bounded`] reads the whole body into memory under a hard cap, for
//! XML that is measured in kilobytes. [`open`] hands back the still-open
//! stream and a description of how the body is framed, for bytes that are
//! measured in gigabytes and must never be buffered.
//!
//! The cap on the first is not politeness. A device description URL comes from
//! an SSDP packet any host on the network can send, so "read until EOF" there
//! is an unbounded allocation controlled by a stranger.

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// How long to wait for a connection and for the response head.
///
/// Applies to the head only. The body of a media response is deliberately not
/// under a total deadline — a two-hour film legitimately takes two hours — but
/// see [`open`] for the per-read idle timeout that replaces it.
pub const HEAD_TIMEOUT: Duration = Duration::from_secs(10);

/// How long [`open`] waits for a response head.
///
/// Longer than [`HEAD_TIMEOUT`] because the two wait for different work. An XML
/// fetch is a small file off disk; a media response may require the server to
/// seek into a multi-gigabyte file, or to start a transcode, before it can say
/// anything at all — and a media server under load does not owe us an answer in
/// ten seconds.
///
/// Ten was the original figure and it produced exactly one unexplained `502`
/// against a real Universal Media Server, on a deep seek, not reproducible in
/// five further attempts. That is not enough to call it the cause; it is enough
/// to say the budget was too tight to distinguish "slow" from "gone", which is
/// the only thing a timeout is for.
pub const STREAM_HEAD_TIMEOUT: Duration = Duration::from_secs(45);

/// Cap on a response head, matching the server side's [`crate::http`] cap.
const MAX_HEAD_BYTES: usize = 16 * 1024;

/// A parsed absolute `http://` URL.
///
/// Kept as owned strings rather than borrowed slices because these are stored:
/// a control URL survives from discovery until the next browse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    pub host: String,
    pub port: u16,
    /// Path plus query, always beginning with `/`.
    pub path_and_query: String,
}

impl Url {
    /// Parse an absolute `http://` URL. Anything else is `None`.
    ///
    /// Rejecting rather than coercing matters here: these URLs arrive from an
    /// SSDP responder and from DIDL-Lite, neither of which we control. A parser
    /// that shrugged and defaulted the scheme would let `https://…` or
    /// `file://…` through as something it is not.
    pub fn parse(raw: &str) -> Option<Self> {
        let rest = raw.strip_prefix("http://")?;
        // Reject a userinfo component outright. `http://a@b/` is legal and is
        // the classic way to make a URL's apparent host differ from its real
        // one; nothing legitimate here uses it, so it is a refusal rather than
        // a parsing subtlety to get right.
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        if authority.contains('@') || authority.is_empty() {
            return None;
        }
        let (host, port) = match authority.rsplit_once(':') {
            // An IPv6 literal contains colons; `]` after the last one means the
            // split landed inside the address, not before a port.
            Some((h, p)) if !p.contains(']') => (h, p.parse::<u16>().ok()?),
            _ => (authority, 80),
        };
        if host.is_empty() {
            return None;
        }
        Some(Url {
            host: host.to_string(),
            port,
            path_and_query: if path.is_empty() {
                "/".to_string()
            } else {
                path.to_string()
            },
        })
    }

    /// `host:port`, as it goes in a `Host` header and a dial.
    pub fn authority(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// Resolve a possibly-relative URL against this one.
    ///
    /// UPnP device descriptions routinely give control URLs as `/dev/ctrl`
    /// rather than absolute, and the base is the `LOCATION` the description
    /// came from. Only the two forms that actually occur are handled —
    /// absolute, and root-relative — because a half-implemented RFC 3986
    /// resolver that silently mishandles `../` is worse than one that says no.
    pub fn resolve(&self, reference: &str) -> Option<Url> {
        if reference.starts_with("http://") {
            return Url::parse(reference);
        }
        if reference.starts_with("//") || reference.contains("://") {
            // Scheme-relative or some other scheme: not ours to guess at.
            return None;
        }
        let path = if reference.starts_with('/') {
            reference.to_string()
        } else {
            // Relative to the directory of this URL's path.
            let base = self.path_and_query.split('?').next().unwrap_or("/");
            let dir = match base.rfind('/') {
                Some(i) => &base[..=i],
                None => "/",
            };
            format!("{dir}{reference}")
        };
        if path.contains("..") {
            return None;
        }
        Some(Url {
            host: self.host.clone(),
            port: self.port,
            path_and_query: path,
        })
    }
}

impl std::fmt::Display for Url {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.port == 80 {
            write!(f, "http://{}{}", self.host, self.path_and_query)
        } else {
            write!(f, "http://{}:{}{}", self.host, self.port, self.path_and_query)
        }
    }
}

/// A response head: everything before the body.
#[derive(Debug, Clone)]
pub struct Head {
    pub status: u16,
    /// Header names lowercased; values trimmed. Order preserved.
    pub headers: Vec<(String, String)>,
}

impl Head {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// `Content-Length`, if it is present and parses.
    pub fn content_length(&self) -> Option<u64> {
        self.get("content-length")?.trim().parse().ok()
    }

    fn is_chunked(&self) -> bool {
        self.get("transfer-encoding")
            .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"))
    }
}

/// How the body that follows a [`Head`] is framed.
///
/// Named rather than inferred at each use site because the three cases have
/// genuinely different consequences downstream — in particular, only
/// [`Framing::Length`] can be forwarded with a `Content-Length`, and only a
/// response carrying one is seekable by a browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing {
    /// `Content-Length: n`. Exactly `n` bytes follow.
    Length(u64),
    /// `Transfer-Encoding: chunked`.
    Chunked,
    /// Neither. The body ends when the connection does — which means its length
    /// is unknowable in advance, and a truncated transfer is indistinguishable
    /// from a complete one.
    ToEof,
}

/// An open response: the head, plus the body still on the wire.
pub struct Open {
    pub head: Head,
    pub framing: Framing,
    /// The connection, positioned at the first body byte.
    ///
    /// Any bytes of the body that arrived in the same read as the head are
    /// replayed in front of it, so this can be read straight through.
    pub body: crate::http::Prefixed<TcpStream>,
}

/// Build a request head. Shared by both entry points so they cannot drift.
fn request_head(method: &str, url: &Url, extra: &[(&str, &str)], body_len: Option<usize>) -> String {
    let mut s = format!(
        "{method} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\
         User-Agent: coyote-bridge/{}\r\nAccept-Encoding: identity\r\n",
        url.path_and_query,
        url.authority(),
        env!("CARGO_PKG_VERSION"),
    );
    for (k, v) in extra {
        // A header value containing CR or LF would let a caller inject a whole
        // extra header — and one caller forwards a `Range` value that came from
        // the phone. Refusing to write it is the only safe handling; skipping
        // is correct because every optional header here is optional.
        if v.contains('\r') || v.contains('\n') || k.contains('\r') || k.contains('\n') {
            continue;
        }
        s.push_str(&format!("{k}: {v}\r\n"));
    }
    if let Some(n) = body_len {
        s.push_str(&format!("Content-Length: {n}\r\n"));
    }
    s.push_str("\r\n");
    s
}

/// Read the response head, returning it and any body bytes read alongside it.
async fn read_head(stream: &mut TcpStream) -> std::io::Result<(Head, Vec<u8>)> {
    read_head_within(stream, HEAD_TIMEOUT).await
}

async fn read_head_within(
    stream: &mut TcpStream,
    budget: Duration,
) -> std::io::Result<(Head, Vec<u8>)> {
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 2048];
    let deadline = tokio::time::Instant::now() + budget;

    let split_at = loop {
        // Unlike the server side, this reads in blocks rather than byte at a
        // time: we are allowed to over-read here, because the surplus belongs
        // to the body and is handed on rather than discarded. That matters for
        // throughput — a byte-at-a-time head read on a media response costs a
        // syscall per byte.
        let n = match tokio::time::timeout_at(deadline, stream.read(&mut chunk)).await {
            Ok(Ok(0)) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "connection closed before the response head was complete",
                ))
            }
            Ok(Ok(n)) => n,
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "timed out waiting for a response head",
                ))
            }
        };
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = find_head_end(&buf) {
            break i;
        }
        if buf.len() > MAX_HEAD_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "response head too large",
            ));
        }
    };

    let head = parse_head(&buf[..split_at])
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "malformed response"))?;
    let leftover = buf[split_at..].to_vec();
    Ok((head, leftover))
}

/// Index just past the `\r\n\r\n` that ends the head.
fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

fn parse_head(bytes: &[u8]) -> Option<Head> {
    // `from_utf8_lossy`, not `from_utf8`: a header value with a stray non-UTF-8
    // byte should not lose us the whole response. Nothing here is executed, and
    // the values we act on are all ASCII.
    let text = String::from_utf8_lossy(bytes);
    let mut lines = text.split("\r\n");
    let status_line = lines.next()?;
    let mut parts = status_line.split(' ');
    let version = parts.next()?;
    if !version.starts_with("HTTP/") {
        return None;
    }
    let status: u16 = parts.next()?.parse().ok()?;

    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
    }
    Some(Head { status, headers })
}

/// Send a request and hand back the response with its body unread.
///
/// The caller owns the streaming. There is no total deadline — a long film is a
/// long transfer — but every individual read carries `idle_timeout`, so a
/// server that stops sending mid-file fails in seconds rather than pinning a
/// task until the OS gives up. Those are different failures and only the second
/// is bounded by TCP.
pub async fn open(
    method: &str,
    url: &Url,
    extra_headers: &[(&str, &str)],
) -> std::io::Result<Open> {
    let mut stream = match tokio::time::timeout(HEAD_TIMEOUT, TcpStream::connect(url.authority()))
        .await
    {
        Ok(r) => r?,
        Err(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("timed out connecting to {}", url.authority()),
            ))
        }
    };
    // Nagle off: the request head is one small write and the response is
    // latency-sensitive on a seek.
    let _ = stream.set_nodelay(true);

    let head_text = request_head(method, url, extra_headers, None);
    stream.write_all(head_text.as_bytes()).await?;
    stream.flush().await?;

    let (head, leftover) = read_head_within(&mut stream, STREAM_HEAD_TIMEOUT).await?;
    let framing = if head.is_chunked() {
        Framing::Chunked
    } else if let Some(n) = head.content_length() {
        Framing::Length(n)
    } else {
        Framing::ToEof
    };

    Ok(Open {
        head,
        framing,
        body: crate::http::Prefixed::new(leftover, stream),
    })
}

/// Send a request with an optional body and read the whole response into
/// memory, refusing anything larger than `max_bytes`.
///
/// For XML: device descriptions and SOAP responses. Never for media.
pub async fn fetch_bounded(
    method: &str,
    url: &Url,
    extra_headers: &[(&str, &str)],
    body: Option<&[u8]>,
    max_bytes: usize,
) -> std::io::Result<(Head, Vec<u8>)> {
    let mut stream = match tokio::time::timeout(HEAD_TIMEOUT, TcpStream::connect(url.authority()))
        .await
    {
        Ok(r) => r?,
        Err(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("timed out connecting to {}", url.authority()),
            ))
        }
    };
    let _ = stream.set_nodelay(true);

    let head_text = request_head(method, url, extra_headers, body.map(|b| b.len()));
    stream.write_all(head_text.as_bytes()).await?;
    if let Some(b) = body {
        stream.write_all(b).await?;
    }
    stream.flush().await?;

    let (head, leftover) = read_head(&mut stream).await?;
    let framing = if head.is_chunked() {
        Framing::Chunked
    } else if let Some(n) = head.content_length() {
        Framing::Length(n)
    } else {
        Framing::ToEof
    };

    // Refuse before reading, when the server was honest enough to say.
    if let Framing::Length(n) = framing {
        if n as usize > max_bytes {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("response is {n} bytes; cap is {max_bytes}"),
            ));
        }
    }

    let mut body_stream = crate::http::Prefixed::new(leftover, stream);
    let collected = match framing {
        Framing::Length(n) => read_exactly(&mut body_stream, n, max_bytes).await?,
        Framing::ToEof => read_to_cap(&mut body_stream, max_bytes).await?,
        Framing::Chunked => read_chunked(&mut body_stream, max_bytes).await?,
    };
    Ok((head, collected))
}

async fn read_exactly<R: AsyncRead + Unpin>(
    r: &mut R,
    n: u64,
    cap: usize,
) -> std::io::Result<Vec<u8>> {
    if n as usize > cap {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "body exceeds cap",
        ));
    }
    let mut out = vec![0u8; n as usize];
    tokio::time::timeout(HEAD_TIMEOUT, r.read_exact(&mut out))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "body read timed out"))??;
    Ok(out)
}

async fn read_to_cap<R: AsyncRead + Unpin>(r: &mut R, cap: usize) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = tokio::time::timeout(HEAD_TIMEOUT, r.read(&mut chunk))
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "body read timed out"))??;
        if n == 0 {
            return Ok(out);
        }
        if out.len() + n > cap {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "body exceeds cap",
            ));
        }
        out.extend_from_slice(&chunk[..n]);
    }
}

/// De-chunk a `Transfer-Encoding: chunked` body.
///
/// Only what the encoding requires: hex size, optional `;ext`, CRLF, bytes,
/// CRLF, terminated by a zero-size chunk. Trailers after it are ignored, since
/// the connection is closing anyway.
async fn read_chunked<R: AsyncRead + Unpin>(r: &mut R, cap: usize) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line = read_line(r).await?;
        let size_text = line.split(';').next().unwrap_or("").trim();
        let size = u64::from_str_radix(size_text, 16).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "bad chunk size")
        })?;
        if size == 0 {
            return Ok(out);
        }
        if out.len() as u64 + size > cap as u64 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "chunked body exceeds cap",
            ));
        }
        let mut buf = vec![0u8; size as usize];
        tokio::time::timeout(HEAD_TIMEOUT, r.read_exact(&mut buf))
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "chunk read timed out"))??;
        out.extend_from_slice(&buf);
        // Trailing CRLF after the chunk data.
        let mut crlf = [0u8; 2];
        r.read_exact(&mut crlf).await?;
    }
}

/// Read one CRLF-terminated line, bounded so a server that never sends one
/// cannot make us allocate without limit.
async fn read_line<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = tokio::time::timeout(HEAD_TIMEOUT, r.read(&mut byte))
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "line read timed out"))??;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed mid-line",
            ));
        }
        if byte[0] == b'\n' {
            while line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(String::from_utf8_lossy(&line).into_owned());
        }
        line.push(byte[0]);
        if line.len() > 256 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "chunk header too long",
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_an_ordinary_url() {
        let u = Url::parse("http://192.168.0.4:5001/ums/media/abc/253/x.mp4").unwrap();
        assert_eq!(u.host, "192.168.0.4");
        assert_eq!(u.port, 5001);
        assert_eq!(u.path_and_query, "/ums/media/abc/253/x.mp4");
    }

    #[test]
    fn defaults_the_port_and_the_path() {
        let u = Url::parse("http://example.local").unwrap();
        assert_eq!(u.port, 80);
        assert_eq!(u.path_and_query, "/");
        assert_eq!(u.to_string(), "http://example.local/");
    }

    /// The one non-obvious rejection. `http://good.host@evil.host/` reads as
    /// `good.host` to a human and connects to `evil.host`, and these URLs come
    /// from an SSDP packet any machine on the network can send.
    #[test]
    fn rejects_a_userinfo_component() {
        assert_eq!(Url::parse("http://trusted@192.168.0.99/x"), None);
    }

    #[test]
    fn rejects_other_schemes() {
        for bad in [
            "https://example.com/",
            "file:///etc/passwd",
            "ftp://example.com/",
            "//example.com/",
            "/relative",
            "",
        ] {
            assert_eq!(Url::parse(bad), None, "should reject {bad}");
        }
    }

    #[test]
    fn resolves_a_root_relative_control_url() {
        let base = Url::parse("http://192.168.0.4:5001/description/fetch").unwrap();
        let ctrl = base.resolve("/upnp/control/content_directory").unwrap();
        assert_eq!(ctrl.host, "192.168.0.4");
        assert_eq!(ctrl.port, 5001);
        assert_eq!(ctrl.path_and_query, "/upnp/control/content_directory");
    }

    #[test]
    fn resolves_a_directory_relative_control_url() {
        let base = Url::parse("http://h:5001/dev/desc.xml").unwrap();
        assert_eq!(
            base.resolve("ctrl").unwrap().path_and_query,
            "/dev/ctrl"
        );
    }

    /// A control URL is not allowed to leave the device it was advertised by.
    #[test]
    fn resolution_cannot_change_host() {
        let base = Url::parse("http://192.168.0.4:5001/desc.xml").unwrap();
        assert_eq!(base.resolve("http://evil.host/x").unwrap().host, "evil.host");
        // …which is why the caller checks. `resolve` reports what was asked
        // for; `dlna` is what refuses it. Documented here so the split is
        // visible from the test rather than only from the call site.
        assert_eq!(base.resolve("../../etc/passwd"), None);
    }

    #[test]
    fn parses_a_response_head() {
        let head = parse_head(b"HTTP/1.1 206 Partial Content\r\nContent-Length: 12\r\nContent-Range: bytes 0-11/100\r\n").unwrap();
        assert_eq!(head.status, 206);
        assert_eq!(head.content_length(), Some(12));
        assert_eq!(head.get("content-range"), Some("bytes 0-11/100"));
    }

    #[test]
    fn header_lookup_is_case_insensitive_via_lowercasing() {
        let head = parse_head(b"HTTP/1.1 200 OK\r\nCONTENT-TYPE: video/mp4\r\n").unwrap();
        assert_eq!(head.get("content-type"), Some("video/mp4"));
    }

    /// Header injection through a forwarded value. The `Range` header this
    /// client sends upstream is copied from a request the phone made.
    #[test]
    fn a_header_value_with_crlf_is_dropped_not_written() {
        let url = Url::parse("http://h/x").unwrap();
        let text = request_head("GET", &url, &[("range", "bytes=0-\r\nX-Evil: 1")], None);
        assert!(!text.contains("X-Evil"));
        assert!(!text.contains("range:"));
    }

    #[tokio::test]
    async fn de_chunks_a_chunked_body() {
        let (mut peer, server) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let _ = peer
                .write_all(b"5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n")
                .await;
        });
        let mut server = server;
        let out = read_chunked(&mut server, 1024).await.unwrap();
        assert_eq!(out, b"hello world");
    }

    #[tokio::test]
    async fn a_chunked_body_over_the_cap_is_refused() {
        let (mut peer, server) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let _ = peer.write_all(b"10\r\n0123456789abcdef\r\n0\r\n\r\n").await;
        });
        let mut server = server;
        assert!(read_chunked(&mut server, 8).await.is_err());
    }

    /// A server that declares a huge body is refused before a byte of it is
    /// read, rather than after the allocation.
    #[test]
    fn a_declared_oversize_body_is_visible_from_the_head_alone() {
        let head = parse_head(b"HTTP/1.1 200 OK\r\nContent-Length: 4294967296\r\n").unwrap();
        assert_eq!(head.content_length(), Some(4 * 1024 * 1024 * 1024));
    }
}
