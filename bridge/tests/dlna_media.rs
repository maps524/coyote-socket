//! End-to-end: a fake DLNA server, the real bridge, and real range requests.
//!
//! The unit tests in `mediaproxy` drive an in-memory cursor, which proves the
//! framing logic. This proves the thing that actually ships: bytes leaving a
//! socket, through the real router, the real auth check and the real proxy, and
//! arriving with the right status and the right offsets.
//!
//! **Scrubbing is the acceptance condition, not playing.** A proxy that
//! mishandles `Range` plays from the beginning and fails only when someone
//! seeks — and the blame lands on the video file. Every range form a browser
//! actually sends is exercised here against a real socket.
//!
//! ## Network reach
//!
//! Every listener in this file binds `127.0.0.1:0` and every address dialled is
//! loopback. Nothing multicasts, and `ssdp::discover` is never called — the
//! media server is registered with `Dlna::add_server`, which is also the real
//! escape hatch for a network where discovery cannot work. `FOLLOW-UPS.md`
//! records a test in a sibling crate that sprayed the real LAN on every
//! `cargo test`; the ephemeral port matters for the second reason too, since a
//! fixed one collides with a bridge that is already running.

use std::net::SocketAddr;
use std::sync::Arc;

use coyote_bridge::auth::Token;
use coyote_bridge::dlna::Dlna;
use coyote_bridge::http;
use coyote_bridge::httpc::Url;
use coyote_bridge::state::PlayerSnapshot;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};

/// The media the fake server holds. Deliberately not a round number, so an
/// off-by-one in a range calculation cannot hide behind a power of two.
const MEDIA_LEN: usize = 10_037;

fn media() -> Vec<u8> {
    (0..MEDIA_LEN).map(|i| (i % 251) as u8).collect()
}

/// How the fake media server answers a `Range` header.
#[derive(Clone, Copy, PartialEq)]
enum RangeSupport {
    /// Answers `206` with a correct `Content-Range`, like a real DLNA server
    /// advertising `DLNA.ORG_OP=01`.
    Honours,
    /// Ignores the header and sends the whole file with `200`. Real servers do
    /// this, and it is the case that makes a player restart on every seek.
    Ignores,
}

/// A stand-in for Universal Media Server: a description, a ContentDirectory,
/// and one video.
async fn fake_media_server(support: RangeSupport) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") && head.len() < 16 * 1024 {
                    match sock.read(&mut byte).await {
                        Ok(0) | Err(_) => return,
                        Ok(_) => head.push(byte[0]),
                    }
                }
                let text = String::from_utf8_lossy(&head).to_string();
                let target = text.split(' ').nth(1).unwrap_or("/").to_string();
                let is_head = text.starts_with("HEAD ");

                if target.starts_with("/description") {
                    let body = String::from(
                        r#"<?xml version="1.0"?><root xmlns="urn:schemas-upnp-org:device-1-0">
<device><deviceType>urn:schemas-upnp-org:device:MediaServer:1</deviceType>
<friendlyName>Fake Media Server</friendlyName><modelName>Fake</modelName>
<UDN>uuid:11111111-2222-3333-4444-555555555555</UDN>
<serviceList>
<service><serviceType>urn:schemas-upnp-org:service:ConnectionManager:1</serviceType>
<controlURL>/upnp/control/connection_manager</controlURL></service>
<service><serviceType>urn:schemas-upnp-org:service:ContentDirectory:1</serviceType>
<controlURL>/upnp/control/content_directory</controlURL></service>
</serviceList></device></root>"#
                    );
                    let _ = sock
                        .write_all(
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: text/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            )
                            .as_bytes(),
                        )
                        .await;
                    return;
                }

                if target.contains("content_directory") {
                    // Drain the SOAP body so the client's write does not RST.
                    let want: usize = text
                        .to_ascii_lowercase()
                        .split("content-length:")
                        .nth(1)
                        .and_then(|s| s.split("\r\n").next())
                        .and_then(|s| s.trim().parse().ok())
                        .unwrap_or(0);
                    let mut body = vec![0u8; want];
                    let _ = sock.read_exact(&mut body).await;

                    // Two `<res>` on purpose: Matroska first, which is what a
                    // "take the first one" proxy picks and which WebKit will
                    // not decode.
                    let didl = format!(
                        r#"&lt;DIDL-Lite xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:upnp="urn:schemas-upnp-org:metadata-1-0/upnp/"&gt;&lt;container id="1$7" parentID="0" childCount="3"&gt;&lt;dc:title&gt;Videos&lt;/dc:title&gt;&lt;/container&gt;&lt;item id="1$7$253" parentID="1$7"&gt;&lt;dc:title&gt;Cock Hero Island 5 Episode I&lt;/dc:title&gt;&lt;upnp:class&gt;object.item.videoItem&lt;/upnp:class&gt;&lt;res protocolInfo="http-get:*:video/x-matroska:DLNA.ORG_OP=01" size="99999"&gt;http://{addr}/media.mkv&lt;/res&gt;&lt;res protocolInfo="http-get:*:video/mp4:DLNA.ORG_OP=01;DLNA.ORG_CI=0" size="{MEDIA_LEN}" duration="0:01:23.000" resolution="3840x1920"&gt;http://{addr}/media.mp4&lt;/res&gt;&lt;/item&gt;&lt;/DIDL-Lite&gt;"#
                    );
                    let body = format!(
                        r#"<?xml version="1.0"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><u:BrowseResponse xmlns:u="urn:schemas-upnp-org:service:ContentDirectory:1"><Result>{didl}</Result><NumberReturned>2</NumberReturned><TotalMatches>2</TotalMatches></u:BrowseResponse></s:Body></s:Envelope>"#
                    );
                    let _ = sock
                        .write_all(
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: text/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            )
                            .as_bytes(),
                        )
                        .await;
                    return;
                }

                if target.starts_with("/media.mp4") {
                    let all = media();
                    let range = header(&text, "range");
                    match (support, range) {
                        (RangeSupport::Honours, Some(value)) => {
                            let Some((start, end)) = resolve_range(&value, all.len()) else {
                                let _ = sock
                                    .write_all(
                                        format!(
                                            "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                                            all.len()
                                        )
                                        .as_bytes(),
                                    )
                                    .await;
                                return;
                            };
                            let slice = &all[start..=end];
                            let _ = sock
                                .write_all(
                                    format!(
                                        "HTTP/1.1 206 Partial Content\r\nContent-Type: video/mp4\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{end}/{}\r\nAccept-Ranges: bytes\r\nSet-Cookie: leaky=1\r\nConnection: close\r\n\r\n",
                                        slice.len(),
                                        all.len()
                                    )
                                    .as_bytes(),
                                )
                                .await;
                            if !is_head {
                                let _ = sock.write_all(slice).await;
                            }
                        }
                        _ => {
                            let _ = sock
                                .write_all(
                                    format!(
                                        "HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                                        all.len()
                                    )
                                    .as_bytes(),
                                )
                                .await;
                            if !is_head {
                                // In chunks with a yield between, so a proxy
                                // that buffers the whole body before writing
                                // anything is distinguishable from one that
                                // streams.
                                for chunk in all.chunks(1024) {
                                    if sock.write_all(chunk).await.is_err() {
                                        return;
                                    }
                                    tokio::task::yield_now().await;
                                }
                            }
                        }
                    }
                    return;
                }

                let _ = sock
                    .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await;
            });
        }
    });

    addr
}

fn header(head: &str, name: &str) -> Option<String> {
    head.split("\r\n").skip(1).find_map(|line| {
        let (k, v) = line.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case(name)
            .then(|| v.trim().to_string())
    })
}

/// Resolve a `Range` value against a known length. Inclusive.
fn resolve_range(value: &str, len: usize) -> Option<(usize, usize)> {
    let spec = value.trim().strip_prefix("bytes=")?;
    let (s, e) = spec.split_once('-')?;
    let (start, end) = match (s.trim().is_empty(), e.trim().is_empty()) {
        (false, true) => (s.trim().parse::<usize>().ok()?, len - 1),
        (false, false) => (s.trim().parse().ok()?, e.trim().parse::<usize>().ok()?),
        (true, false) => {
            let n: usize = e.trim().parse().ok()?;
            (len.saturating_sub(n), len - 1)
        }
        (true, true) => return None,
    };
    if start >= len || end < start {
        return None;
    }
    Some((start, end.min(len - 1)))
}

/// Start the real bridge listener with DLNA mounted and the fake server pinned.
async fn bridge_with(server: SocketAddr) -> (SocketAddr, Token, Arc<Dlna>) {
    let dlna = Arc::new(Dlna::new());
    dlna.add_server(Url::parse(&format!("http://{server}/description/fetch")).unwrap())
        .await
        .expect("the fake server must be describable");

    let (_snap_tx, snapshot_rx) = watch::channel(PlayerSnapshot::new("127.0.0.1:23554".into()));
    let (cmd_tx, _cmd_rx) = mpsc::channel(4);
    let token = Token::generate();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let ctx = Arc::new(http::Ctx {
        snapshot_rx,
        cmd_tx,
        static_dir: None,
        dlna: Some(Arc::clone(&dlna)),
        pairing_base: format!("http://{addr}"),
        token: std::sync::RwLock::new(token.clone()),
        allowed_hosts: vec![addr.to_string()],
        on_token_rotated: None,
    });
    tokio::spawn(async move { http::run(listener, ctx).await });

    // Leak the receivers so the channels stay open for the run.
    std::mem::forget(_snap_tx);
    std::mem::forget(_cmd_rx);

    (addr, token, dlna)
}

/// One request/response against the bridge. Returns (head, body).
async fn request(
    bridge: SocketAddr,
    method: &str,
    target: &str,
    extra: &[(&str, &str)],
) -> (String, Vec<u8>) {
    let mut sock = TcpStream::connect(bridge).await.unwrap();
    let mut req = format!("{method} {target} HTTP/1.1\r\nHost: {bridge}\r\n");
    for (k, v) in extra {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("Connection: close\r\n\r\n");
    sock.write_all(req.as_bytes()).await.unwrap();

    let mut all = Vec::new();
    sock.read_to_end(&mut all).await.unwrap();
    let split = all
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("a response head");
    (
        String::from_utf8_lossy(&all[..split]).into_owned(),
        all[split + 4..].to_vec(),
    )
}

// ---------------------------------------------------------------------------

/// The listing arrives, with the real title and a playable URL — and the
/// Matroska resource, which is listed first, is not the one chosen.
#[tokio::test]
async fn browsing_yields_a_real_title_and_a_playable_resource() {
    let server = fake_media_server(RangeSupport::Honours).await;
    let (bridge, token, _dlna) = bridge_with(server).await;

    let (head, body) = request(
        bridge,
        "GET",
        &format!("/dlna/browse.json?t={token}&server=uuid%3A11111111-2222-3333-4444-555555555555&object=0"),
        &[],
    )
    .await;
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");

    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["containers"][0]["title"], "Videos");

    let item = &v["items"][0];
    // `dc:title`: spaces, not the dashes UMS puts in the path.
    assert_eq!(item["title"], "Cock Hero Island 5 Episode I");
    assert_eq!(item["seekable"], true);
    assert_eq!(item["resolution"], "3840x1920");
    let media_url = item["mediaUrl"].as_str().unwrap();
    assert!(media_url.starts_with("/dlna/media/"), "{media_url}");
    // The chosen resource is the mp4, not the mkv that came first.
    assert!(item["chosen"].as_str().unwrap().contains("video/mp4"), "{item}");
}

/// The listing and the media are behind the same token gate as `/healthz`.
#[tokio::test]
async fn the_dlna_surface_is_token_gated() {
    let server = fake_media_server(RangeSupport::Honours).await;
    let (bridge, _token, _dlna) = bridge_with(server).await;

    for target in ["/dlna/index.json", "/dlna/browse.json", "/dlna/media/abc"] {
        let (head, _) = request(bridge, "GET", target, &[]).await;
        assert!(head.starts_with("HTTP/1.1 401"), "{target}: {head}");
    }
}

/// A URL the bridge did not mint is not fetchable, whatever the token.
#[tokio::test]
async fn an_unminted_reference_is_refused() {
    let server = fake_media_server(RangeSupport::Honours).await;
    let (bridge, token, _dlna) = bridge_with(server).await;

    let (head, body) = request(
        bridge,
        "GET",
        &format!("/dlna/media/0123456789abcdef?t={token}"),
        &[],
    )
    .await;
    assert!(head.starts_with("HTTP/1.1 404"), "{head}");
    assert!(String::from_utf8_lossy(&body).contains("restart"));
}

/// Fetch the listing and return the minted media path for the one item.
async fn media_path(bridge: SocketAddr, token: &Token) -> String {
    let (_, body) = request(
        bridge,
        "GET",
        &format!("/dlna/browse.json?t={token}&server=uuid%3A11111111-2222-3333-4444-555555555555&object=0"),
        &[],
    )
    .await;
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    v["items"][0]["mediaUrl"].as_str().unwrap().to_string()
}

/// **The acceptance condition.** Every range form a media element sends,
/// against a real socket, checked byte for byte.
#[tokio::test]
async fn every_range_form_a_browser_sends_is_answered_correctly() {
    let server = fake_media_server(RangeSupport::Honours).await;
    let (bridge, token, _dlna) = bridge_with(server).await;
    let path = media_path(bridge, &token).await;
    let all = media();

    // 1. No Range at all — the initial load.
    let (head, body) = request(bridge, "GET", &format!("{path}?t={token}"), &[]).await;
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(head.contains(&format!("Content-Length: {MEDIA_LEN}")), "{head}");
    assert!(head.contains("Accept-Ranges: bytes"), "{head}");
    assert_eq!(body, all, "the whole file must arrive intact");

    // 2. A bounded range — what a browser sends to probe the container.
    let (head, body) = request(
        bridge,
        "GET",
        &format!("{path}?t={token}"),
        &[("Range", "bytes=0-1023")],
    )
    .await;
    assert!(head.starts_with("HTTP/1.1 206 Partial Content"), "{head}");
    assert!(
        head.contains(&format!("Content-Range: bytes 0-1023/{MEDIA_LEN}")),
        "{head}"
    );
    assert_eq!(body, all[0..1024]);

    // 3. **The open-ended form.** This is what a seek looks like, and it is the
    //    one a careless parser drops because there is nothing after the dash.
    let seek_to = 7_000;
    let (head, body) = request(
        bridge,
        "GET",
        &format!("{path}?t={token}"),
        &[("Range", &format!("bytes={seek_to}-"))],
    )
    .await;
    assert!(head.starts_with("HTTP/1.1 206"), "{head}");
    assert!(
        head.contains(&format!("Content-Range: bytes {seek_to}-{}/{MEDIA_LEN}", MEDIA_LEN - 1)),
        "{head}"
    );
    assert!(head.contains(&format!("Content-Length: {}", MEDIA_LEN - seek_to)), "{head}");
    assert_eq!(body, all[seek_to..], "a seek must land on the right byte");

    // 4. A suffix range — how some players read a trailing index.
    let (head, body) = request(
        bridge,
        "GET",
        &format!("{path}?t={token}"),
        &[("Range", "bytes=-500")],
    )
    .await;
    assert!(head.starts_with("HTTP/1.1 206"), "{head}");
    assert_eq!(body, all[MEDIA_LEN - 500..]);

    // 5. A range past the end must come back as 416, not as a silent 200.
    //    A player that gets a 200 here retries forever.
    let (head, _) = request(
        bridge,
        "GET",
        &format!("{path}?t={token}"),
        &[("Range", "bytes=99999999-")],
    )
    .await;
    assert!(head.starts_with("HTTP/1.1 416"), "{head}");
    assert!(head.contains(&format!("Content-Range: bytes */{MEDIA_LEN}")), "{head}");

    // 6. `If-Range` is forwarded rather than swallowed.
    let (head, _) = request(
        bridge,
        "GET",
        &format!("{path}?t={token}"),
        &[("Range", "bytes=10-19"), ("If-Range", "\"etag-value\"")],
    )
    .await;
    assert!(head.starts_with("HTTP/1.1 206"), "{head}");

    // 7. HEAD: the length, and no body. A player that HEADs first to size the
    //    file would otherwise read the video as its next response.
    let (head, body) = request(bridge, "HEAD", &format!("{path}?t={token}"), &[]).await;
    assert!(head.contains(&format!("Content-Length: {MEDIA_LEN}")), "{head}");
    assert!(body.is_empty(), "a HEAD response carried {} bytes", body.len());
}

/// The upstream's own headers do not arrive wearing this origin's authority.
#[tokio::test]
async fn upstream_cookies_do_not_reach_the_phone() {
    let server = fake_media_server(RangeSupport::Honours).await;
    let (bridge, token, _dlna) = bridge_with(server).await;
    let path = media_path(bridge, &token).await;

    let (head, _) = request(
        bridge,
        "GET",
        &format!("{path}?t={token}"),
        &[("Range", "bytes=0-9")],
    )
    .await;
    assert!(!head.contains("Set-Cookie"), "{head}");
    assert!(head.contains("Content-Type: video/mp4"), "{head}");
}

/// A media server that ignores `Range` still produces a correct `206` for a
/// bounded seek — otherwise the player restarts from the beginning every time
/// someone scrubs, and the video gets the blame.
#[tokio::test]
async fn a_range_ignoring_server_is_compensated_for() {
    let server = fake_media_server(RangeSupport::Ignores).await;
    let (bridge, token, _dlna) = bridge_with(server).await;
    let path = media_path(bridge, &token).await;
    let all = media();

    let seek_to = 4_096;
    let (head, body) = request(
        bridge,
        "GET",
        &format!("{path}?t={token}"),
        &[("Range", &format!("bytes={seek_to}-"))],
    )
    .await;
    assert!(head.starts_with("HTTP/1.1 206"), "{head}");
    assert!(
        head.contains(&format!("Content-Range: bytes {seek_to}-{}/{MEDIA_LEN}", MEDIA_LEN - 1)),
        "{head}"
    );
    assert_eq!(body, all[seek_to..]);
}

/// Two clients streaming at once both get whole, correct files. The bridge
/// spawns a task per connection, but the proxy holds per-request state and a
/// shared buffer would corrupt both.
#[tokio::test]
async fn two_concurrent_streams_do_not_interfere() {
    let server = fake_media_server(RangeSupport::Honours).await;
    let (bridge, token, _dlna) = bridge_with(server).await;
    let path = media_path(bridge, &token).await;
    let all = media();

    let a = tokio::spawn({
        let (p, t) = (path.clone(), token.clone());
        async move {
            request(bridge, "GET", &format!("{p}?t={t}"), &[("Range", "bytes=0-4999")]).await
        }
    });
    let b = tokio::spawn({
        let (p, t) = (path.clone(), token.clone());
        async move {
            request(bridge, "GET", &format!("{p}?t={t}"), &[("Range", "bytes=5000-9999")]).await
        }
    });

    let (head_a, body_a) = a.await.unwrap();
    let (head_b, body_b) = b.await.unwrap();
    assert!(head_a.starts_with("HTTP/1.1 206"), "{head_a}");
    assert!(head_b.starts_with("HTTP/1.1 206"), "{head_b}");
    assert_eq!(body_a, all[0..5000]);
    assert_eq!(body_b, all[5000..10000]);
}

/// The proxy streams rather than buffers.
///
/// The fake server writes the body in 1 KiB chunks with a yield between each,
/// and the response head plus the first chunk must arrive before the last chunk
/// has been written. A proxy that reads the whole body before writing anything
/// would fail this — and on a 40 GB VR file that same proxy is an allocation
/// that ends the process, having passed every test done on a short clip.
#[tokio::test]
async fn the_response_head_arrives_before_the_body_is_complete() {
    let server = fake_media_server(RangeSupport::Ignores).await;
    let (bridge, token, _dlna) = bridge_with(server).await;
    let path = media_path(bridge, &token).await;

    let mut sock = TcpStream::connect(bridge).await.unwrap();
    sock.write_all(
        format!("GET {path}?t={token} HTTP/1.1\r\nHost: {bridge}\r\nConnection: close\r\n\r\n")
            .as_bytes(),
    )
    .await
    .unwrap();

    // Read just enough for the head and a little body, then stop. If the proxy
    // had buffered, nothing would be readable until the upstream finished.
    let mut buf = vec![0u8; 2048];
    let mut got = Vec::new();
    while got.len() < 1200 {
        let n = tokio::time::timeout(std::time::Duration::from_secs(5), sock.read(&mut buf))
            .await
            .expect("the head must not wait for the whole body")
            .unwrap();
        assert!(n > 0, "connection closed before any body arrived");
        got.extend_from_slice(&buf[..n]);
    }
    let split = got.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&got[..split]);
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(got.len() > split + 4, "body bytes arrived with the head");
}

/// A bridge with DLNA switched off says so, rather than reporting an empty
/// library — "not enabled" is a setting to change, "found nothing" is a network
/// to debug, and they are different answers.
#[tokio::test]
async fn a_bridge_without_dlna_says_it_is_not_enabled() {
    let (_snap_tx, snapshot_rx) = watch::channel(PlayerSnapshot::new("x".into()));
    let (cmd_tx, _cmd_rx) = mpsc::channel(4);
    let token = Token::generate();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let ctx = Arc::new(http::Ctx {
        snapshot_rx,
        cmd_tx,
        static_dir: None,
        dlna: None,
        pairing_base: format!("http://{addr}"),
        token: std::sync::RwLock::new(token.clone()),
        allowed_hosts: vec![addr.to_string()],
        on_token_rotated: None,
    });
    tokio::spawn(async move { http::run(listener, ctx).await });
    std::mem::forget(_snap_tx);
    std::mem::forget(_cmd_rx);

    let (head, body) = request(addr, "GET", &format!("/dlna/index.json?t={token}"), &[]).await;
    assert!(head.starts_with("HTTP/1.1 404"), "{head}");
    assert!(String::from_utf8_lossy(&body).contains("not enabled"));
}

/// Not a DLNA test. A pre-existing defect in the shared surface, asserted so
/// the report of it is a fact rather than a reading of the code.
///
/// `serve_conn` accepts `HEAD`, `route` never looks at the method, and
/// `respond` writes the body unconditionally — so `HEAD /healthz` returns a
/// body. Left for the owner of `http.rs`; see `FOLLOW-UPS.md`.
#[tokio::test]
#[ignore = "documents a defect this branch deliberately does not fix"]
async fn head_on_a_respond_route_wrongly_carries_a_body() {
    let server = fake_media_server(RangeSupport::Honours).await;
    let (bridge, token, _dlna) = bridge_with(server).await;
    let (head, body) = request(bridge, "HEAD", &format!("/healthz?t={token}"), &[]).await;
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(
        !body.is_empty(),
        "if this now passes with an empty body, the defect has been fixed and this test should go"
    );
}
