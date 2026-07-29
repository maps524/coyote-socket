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
/// What the fake server puts in its listing.
///
/// The awkward catalogue exists because of `FOLLOW-UPS.md` §0c: a run against
/// the real media server exercised only the paths where everything works, and
/// left the two that explain failure — `unplayable`, and the notice that
/// seeking will not work — as the least-tested code in the change. They are
/// also what a user on a different server meets first. A fixture built to
/// misbehave is the cheapest way to stop that being true.
#[derive(Clone, Copy, PartialEq)]
enum Catalogue {
    /// One item, two `<res>`, both fine. What the real UMS looks like.
    Playable,
    /// What somebody else's server looks like: one item offering only a
    /// container WebKit will not decode, and one that says outright it does not
    /// honour byte ranges.
    Awkward,
    /// One item whose DIDL `size` understates the file — which is what a DLNA
    /// server emits for a transcoded resource, where the length is an estimate.
    Understated,
}

async fn fake_media_server(support: RangeSupport) -> SocketAddr {
    fake_media_server_with(support, Catalogue::Playable).await
}

async fn fake_media_server_with(support: RangeSupport, catalogue: Catalogue) -> SocketAddr {
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
                    let didl = if catalogue == Catalogue::Understated {
                        format!(
                            r#"&lt;DIDL-Lite xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:upnp="urn:schemas-upnp-org:metadata-1-0/upnp/"&gt;&lt;item id="estimated" parentID="0"&gt;&lt;dc:title&gt;A Transcode With An Estimated Length&lt;/dc:title&gt;&lt;upnp:class&gt;object.item.videoItem&lt;/upnp:class&gt;&lt;res protocolInfo="http-get:*:video/mp4:DLNA.ORG_OP=01;DLNA.ORG_CI=1" size="1000"&gt;http://{addr}/media.mp4&lt;/res&gt;&lt;/item&gt;&lt;/DIDL-Lite&gt;"#
                        )
                    } else if catalogue == Catalogue::Awkward {
                        format!(
                            r#"&lt;DIDL-Lite xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:upnp="urn:schemas-upnp-org:metadata-1-0/upnp/"&gt;&lt;item id="mkv-only" parentID="0"&gt;&lt;dc:title&gt;An Old Matroska Rip&lt;/dc:title&gt;&lt;upnp:class&gt;object.item.videoItem&lt;/upnp:class&gt;&lt;res protocolInfo="http-get:*:video/x-matroska:DLNA.ORG_OP=01" size="123456"&gt;http://{addr}/media.mkv&lt;/res&gt;&lt;/item&gt;&lt;item id="no-ranges" parentID="0"&gt;&lt;dc:title&gt;Streamed Without Seeking&lt;/dc:title&gt;&lt;upnp:class&gt;object.item.videoItem&lt;/upnp:class&gt;&lt;res protocolInfo="http-get:*:video/mp4:DLNA.ORG_OP=00" size="{MEDIA_LEN}" duration="0:01:23.000"&gt;http://{addr}/media.mp4&lt;/res&gt;&lt;/item&gt;&lt;/DIDL-Lite&gt;"#
                        )
                    } else {
                        format!(
                        r#"&lt;DIDL-Lite xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:upnp="urn:schemas-upnp-org:metadata-1-0/upnp/"&gt;&lt;container id="1$7" parentID="0" childCount="3"&gt;&lt;dc:title&gt;Videos&lt;/dc:title&gt;&lt;/container&gt;&lt;item id="1$7$253" parentID="1$7"&gt;&lt;dc:title&gt;Cock Hero Island 5 Episode I&lt;/dc:title&gt;&lt;upnp:class&gt;object.item.videoItem&lt;/upnp:class&gt;&lt;res protocolInfo="http-get:*:video/x-matroska:DLNA.ORG_OP=01" size="99999"&gt;http://{addr}/media.mkv&lt;/res&gt;&lt;res protocolInfo="http-get:*:video/mp4:DLNA.ORG_OP=01;DLNA.ORG_CI=0" size="{MEDIA_LEN}" duration="0:01:23.000" resolution="3840x1920"&gt;http://{addr}/media.mp4&lt;/res&gt;&lt;/item&gt;&lt;/DIDL-Lite&gt;"#
                        )
                    };
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
    // The name script matching needs, which the opaque media reference hides.
    // UMS has already turned the title's spaces into dashes here, and it is the
    // dashed form that sits beside the funscript on disk.
    assert_eq!(item["fileName"], "media.mp4");
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

/// **Found against a real Universal Media Server 15.7.0, not reasoned about.**
///
/// A seek past the end of the file made UMS answer `200` with the whole thing:
/// asking "is there anything at byte 99,999,999,999 of this 2 GB video" began a
/// 2 GB download. Relaying that is not merely wasteful — the client asked a
/// question whose answer is "no", and would instead receive the film from the
/// beginning.
///
/// The bridge knows it is unsatisfiable, because the same response carried the
/// length, so it answers `416` itself.
#[tokio::test]
async fn a_seek_past_the_end_is_416_not_the_whole_file() {
    let server = fake_media_server(RangeSupport::Ignores).await;
    let (bridge, token, _dlna) = bridge_with(server).await;
    let path = media_path(bridge, &token).await;

    let (head, body) = request(
        bridge,
        "GET",
        &format!("{path}?t={token}"),
        &[("Range", "bytes=99999999999-")],
    )
    .await;
    // The DIDL `<res>` advertises the same length the origin reports, so the
    // refusal has two independent witnesses. Without that agreement the `200`
    // is relayed instead — see `proxy`.
    assert!(head.starts_with("HTTP/1.1 416"), "{head}");
    assert!(head.contains(&format!("Content-Range: bytes */{MEDIA_LEN}")), "{head}");
    assert!(body.is_empty(), "{} bytes were sent for an unsatisfiable range", body.len());

    // Exactly at the end is also past the end: byte `MEDIA_LEN` does not exist.
    let (head, _) = request(
        bridge,
        "GET",
        &format!("{path}?t={token}"),
        &[("Range", &format!("bytes={MEDIA_LEN}-"))],
    )
    .await;
    assert!(head.starts_with("HTTP/1.1 416"), "{head}");

    // The last byte does exist, and must still be served.
    let (head, body) = request(
        bridge,
        "GET",
        &format!("{path}?t={token}"),
        &[("Range", &format!("bytes={}-", MEDIA_LEN - 1))],
    )
    .await;
    assert!(head.starts_with("HTTP/1.1 206"), "{head}");
    assert_eq!(body, vec![media()[MEDIA_LEN - 1]]);
}

/// The two failure explanations, which a run against the real server never
/// produced — see `FOLLOW-UPS.md` §0c.
///
/// Both items are in the listing rather than dropped from it, and both carry
/// the reason. A library with silent holes in it is one nobody can debug: the
/// question "why is this file missing when the one beside it is not" has to be
/// answerable from the listing, not from a packet capture.
#[tokio::test]
async fn a_server_that_offers_nothing_playable_says_so_per_item() {
    let server = fake_media_server_with(RangeSupport::Honours, Catalogue::Awkward).await;
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
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "neither item may be dropped: {v}");

    // 1. Nothing a browser can decode. No media URL, and the reason names the
    //    container that was on offer rather than saying "unsupported".
    let mkv = items.iter().find(|i| i["id"] == "mkv-only").unwrap();
    assert!(mkv["mediaUrl"].is_null(), "{mkv}");
    let why = mkv["unplayable"].as_str().unwrap();
    assert!(why.contains("video/x-matroska"), "{why}");
    assert_eq!(mkv["title"], "An Old Matroska Rip");

    // 2. Playable, but the server said outright that it does not honour byte
    //    ranges. It still gets a URL — it will play — and `seekable` is what
    //    stops the missing scrub bar being blamed on the bridge.
    let stuck = items.iter().find(|i| i["id"] == "no-ranges").unwrap();
    assert!(stuck["mediaUrl"].is_string(), "{stuck}");
    assert_eq!(stuck["seekable"], false);
    assert!(stuck["unplayable"].is_null(), "it plays; it just cannot seek");
    let chosen = stuck["chosen"].as_str().unwrap();
    assert!(chosen.contains("does NOT honour byte ranges"), "{chosen}");
}

/// And the unseekable item really is served, rather than being refused on the
/// strength of its own advertisement.
///
/// Refusing it would be worse than the missing scrub bar: a file that plays
/// from the start is usable, and a file the bridge declines to serve because of
/// a `protocolInfo` flag is not.
#[tokio::test]
async fn an_unseekable_item_is_still_playable_from_the_start() {
    let server = fake_media_server_with(RangeSupport::Honours, Catalogue::Awkward).await;
    let (bridge, token, _dlna) = bridge_with(server).await;

    let (_, body) = request(
        bridge,
        "GET",
        &format!("/dlna/browse.json?t={token}&server=uuid%3A11111111-2222-3333-4444-555555555555&object=0"),
        &[],
    )
    .await;
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let path = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "no-ranges")
        .unwrap()["mediaUrl"]
        .as_str()
        .unwrap()
        .to_string();

    let (head, bytes) = request(bridge, "GET", &format!("{path}?t={token}"), &[]).await;
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert_eq!(bytes, media());
}

/// **A `416` must not be synthesised from a length only the origin claims.**
///
/// The synthesis branch runs *because the origin ignored `Range`* — it has
/// already shown it is unreliable about ranges, so trusting its
/// `Content-Length` hard enough to refuse on is trusting the wrong witness. A
/// DLNA server emits an estimated length for a transcoded resource, and
/// `score()` will pick a transcode when it is the only playable container.
///
/// Here the DIDL says 1,000 bytes and the file is 10,037. A seek to 2,000 is
/// inside the real file and outside the advertised one, so the old code would
/// have refused a range the origin would have served — playing, then declining
/// to seek past an arbitrary early point, with the client getting the blame.
#[tokio::test]
async fn an_understated_length_does_not_produce_a_spurious_416() {
    let server = fake_media_server_with(RangeSupport::Ignores, Catalogue::Understated).await;
    let (bridge, token, _dlna) = bridge_with(server).await;
    let path = media_path(bridge, &token).await;
    let all = media();

    let seek_to = 2_000;
    let (head, body) = request(
        bridge,
        "GET",
        &format!("{path}?t={token}"),
        &[("Range", &format!("bytes={seek_to}-"))],
    )
    .await;

    assert!(
        !head.starts_with("HTTP/1.1 416"),
        "refused a range the origin would have served: {head}"
    );
    // And the seek still works, by the skip-and-synthesise path.
    assert!(head.starts_with("HTTP/1.1 206"), "{head}");
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

// ---------------------------------------------------------------------------
// The co-located cost
// ---------------------------------------------------------------------------

/// How much the proxy costs when the media server is on the same machine.
///
/// **This is the deployment that matters** — "the bridge lives on the nook
/// where the UMS is operating" — and it is the one that could not be measured
/// against the real server, because the machine to measure it on is the one
/// running UMS. Measured over the network it gave 55 MB/s against 107 MB/s
/// direct, which is the *pessimistic* half: those bytes cross the network
/// twice.
///
/// Here both hops are loopback, so what is left is the proxy itself: a read, a
/// 64 KiB memcpy and a write, against the same read and write without the
/// middle. That is the co-located question with the media server's own disk and
/// software removed — which is the honest thing to measure, because it bounds
/// the proxy's contribution without claiming to predict UMS's.
///
/// Ignored, because a throughput figure is a measurement rather than an
/// assertion and it depends on the machine. Run
/// `cargo test --release --test dlna_media -- --ignored --nocapture
/// measure_the_loopback` and read the output rather than trusting the paragraph
/// in `README.md`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "a measurement, not an assertion — run it with --nocapture"]
async fn measure_the_loopback_proxy_cost() {
    // Large enough that the transfer dominates connection setup.
    const BYTES: u64 = 512 * 1024 * 1024;
    const ROUNDS: usize = 3;

    let origin = fake_bulk_server(BYTES).await;
    let dlna = Arc::new(Dlna::new());
    let device_url = Url::parse(&format!("http://{origin}/desc")).unwrap();
    let (bridge, target) = bridge_for_proxy(&dlna, &device_url, origin).await;

    // One of each, discarded. The first transfer pays for the runtime's worker
    // threads, the first allocations and a cold branch predictor, and charging
    // that to whichever side happens to run first is how a benchmark invents a
    // difference that is not there.
    drain_direct(origin, "/bulk").await;
    drain_bridge(bridge, &target).await;

    let rate = |d: std::time::Duration| BYTES as f64 / d.as_secs_f64() / 1_048_576.0;
    let mut direct = Vec::new();
    let mut via = Vec::new();

    // Alternated, because the two share a machine and whichever runs first gets
    // the quieter one.
    for _ in 0..ROUNDS {
        let started = std::time::Instant::now();
        assert_eq!(drain_direct(origin, "/bulk").await, BYTES);
        direct.push(rate(started.elapsed()));

        let started = std::time::Instant::now();
        let (head, body) = drain_bridge(bridge, &target).await;
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert_eq!(body, BYTES);
        via.push(rate(started.elapsed()));
    }

    let show = |label: &str, runs: &[f64]| {
        let mut sorted = runs.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!(
            "    {label:<24}: {:>6.0} MB/s median   (runs: {})",
            sorted[sorted.len() / 2],
            runs.iter().map(|r| format!("{r:.0}")).collect::<Vec<_>>().join(", ")
        );
    };

    println!("
  loopback, {} MB x {ROUNDS}", BYTES / 1_048_576);
    show("direct from the origin", &direct);
    show("through the proxy", &via);
    println!();
    println!("  A 25 Mb/s VR stream is about 3 MB/s. Both figures are two orders of");
    println!("  magnitude above it, so on loopback the proxy is not the constraint.");
    println!("  That is the only claim this supports.");
    println!();
    println!("  Note the proxy is consistently *faster* than the direct read, which");
    println!("  means the direct figure is not a baseline: reading one socket in");
    println!("  lockstep with the origin's writes is slower than reading from a");
    println!("  proxy that has already buffered ahead. The bottleneck is this test's");
    println!("  reader, not the origin and not the proxy. Do not quote a ratio.");
    println!();
}

/// An origin that serves `total` generated bytes and a device description, and
/// nothing else. Writes from one reused buffer so the origin is not the
/// bottleneck.
async fn fake_bulk_server(total: u64) -> SocketAddr {
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

                if text.contains("/desc") {
                    let body = concat!(
                        "<root><device><UDN>uuid:bulk</UDN>",
                        "<friendlyName>Bulk</friendlyName><serviceList><service>",
                        "<serviceType>urn:schemas-upnp-org:service:ContentDirectory:1</serviceType>",
                        "<controlURL>/ctrl</controlURL></service></serviceList></device></root>"
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

                let _ = sock
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nContent-Length: {total}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .await;
                let chunk = vec![7u8; 64 * 1024];
                let mut left = total;
                while left > 0 {
                    let n = left.min(chunk.len() as u64) as usize;
                    if sock.write_all(&chunk[..n]).await.is_err() {
                        return;
                    }
                    left -= n as u64;
                }
            });
        }
    });
    addr
}

/// Bring up a bridge whose reference table holds one entry for the bulk URL.
async fn bridge_for_proxy(
    dlna: &Arc<Dlna>,
    device_url: &Url,
    origin: SocketAddr,
) -> (SocketAddr, String) {
    let device = dlna.add_server(device_url.clone()).await.unwrap();
    let reference = dlna
        .mint_media_ref(&device, "bulk", "bulk", &format!("http://{origin}/bulk"), None)
        .await
        .expect("the bulk URL is on the device's own host");

    let (_snap_tx, snapshot_rx) = watch::channel(PlayerSnapshot::new("x".into()));
    let (cmd_tx, _cmd_rx) = mpsc::channel(4);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let ctx = Arc::new(http::Ctx {
        snapshot_rx,
        cmd_tx,
        static_dir: None,
        dlna: Some(Arc::clone(dlna)),
        pairing_base: format!("http://{addr}"),
        token: std::sync::RwLock::new(Token::from_string("bench".into())),
        allowed_hosts: vec![addr.to_string()],
        on_token_rotated: None,
    });
    tokio::spawn(async move { http::run(listener, ctx).await });
    std::mem::forget(_snap_tx);
    std::mem::forget(_cmd_rx);
    (addr, format!("/dlna/media/{reference}?t=bench"))
}

async fn drain_direct(origin: SocketAddr, path: &str) -> u64 {
    let mut sock = TcpStream::connect(origin).await.unwrap();
    sock.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: {origin}\r\nConnection: close\r\n\r\n").as_bytes(),
    )
    .await
    .unwrap();
    count_after_head(&mut sock).await.1
}

async fn drain_bridge(bridge: SocketAddr, target: &str) -> (String, u64) {
    let mut sock = TcpStream::connect(bridge).await.unwrap();
    sock.write_all(
        format!("GET {target} HTTP/1.1\r\nHost: {bridge}\r\nConnection: close\r\n\r\n").as_bytes(),
    )
    .await
    .unwrap();
    count_after_head(&mut sock).await
}

/// Read to EOF, keeping the head and counting the body.
async fn count_after_head(sock: &mut TcpStream) -> (String, u64) {
    let mut buf = vec![0u8; 256 * 1024];
    let mut head: Option<String> = None;
    let mut body = 0u64;
    loop {
        let n = sock.read(&mut buf).await.unwrap();
        if n == 0 {
            break;
        }
        match head {
            None => {
                // The origins here write the head in one go, so a straddled
                // head is a panic rather than a silently wrong measurement.
                let i = buf[..n]
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .expect("response head did not arrive in one read");
                head = Some(String::from_utf8_lossy(&buf[..i]).into_owned());
                body += (n - i - 4) as u64;
            }
            Some(_) => body += n as u64,
        }
    }
    (head.unwrap_or_default(), body)
}
