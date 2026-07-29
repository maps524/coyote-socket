//! End-to-end tests: the real 23554 client against the fake player, and the
//! real HTTP/WebSocket surface against a real WebSocket client.
//!
//! **What this does and does not prove.** Everything here runs the production
//! code paths — `player::run` dials a real TCP socket, `codec` frames real
//! bytes, `http::run` serves a real WebSocket. The one substitution is the
//! peer: `fake_player` stands in for a Quest. So these tests prove the client
//! is *internally consistent and robust*, not that the wire format matches
//! what DeoVR actually emits. A shared misreading of the spec would pass every
//! test in this file. See `codec.rs` for where the format came from.

use std::time::Duration;

use coyote_bridge::fake_player::{serve_client, FakePlayerConfig};
use coyote_bridge::http;
use coyote_bridge::state::{LinkState, PlayerCommand, PlayerSnapshot};
use coyote_bridge::auth::Token;
use coyote_bridge::wire::Tap;
use futures::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};

/// Generous enough for a loaded CI box, short enough to fail fast.
const SETTLE: Duration = Duration::from_secs(10);

fn fast_player() -> FakePlayerConfig {
    FakePlayerConfig {
        tick: Duration::from_millis(50),
        full_packet_every: 4,
        ..Default::default()
    }
}

/// Listen on an ephemeral port, serving `limit` sequential clients.
async fn spawn_fake_player(
    cfg: FakePlayerConfig,
    limit: usize,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let handle = tokio::spawn(async move {
        for _ in 0..limit {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let reason = serve_client(stream, &cfg, &Tap::disabled()).await;
            eprintln!("[test] fake player finished with a client: {reason}");
        }
        // Dropping the listener here is deliberate: the reconnect test needs
        // the port to actually go away.
    });
    (addr, handle)
}

/// Start the bridge's player client against `endpoint`.
fn spawn_bridge_client(
    endpoint: String,
) -> (watch::Receiver<PlayerSnapshot>, mpsc::Sender<PlayerCommand>) {
    let (snapshot_tx, snapshot_rx) = watch::channel(PlayerSnapshot::new(endpoint.clone()));
    let (cmd_tx, mut cmd_rx) = mpsc::channel(16);
    tokio::spawn(async move {
        coyote_bridge::player::run(endpoint, &snapshot_tx, &mut cmd_rx, Tap::disabled()).await
    });
    (snapshot_rx, cmd_tx)
}

/// Wait until `pred` holds, or fail with the last snapshot seen.
async fn wait_for(
    rx: &mut watch::Receiver<PlayerSnapshot>,
    what: &str,
    mut pred: impl FnMut(&PlayerSnapshot) -> bool,
) -> PlayerSnapshot {
    let deadline = tokio::time::Instant::now() + SETTLE;
    loop {
        {
            let snap = rx.borrow_and_update().clone();
            if pred(&snap) {
                return snap;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("timed out waiting for {what}; last snapshot was {snap:?}");
            }
        }
        let _ = tokio::time::timeout(Duration::from_millis(500), rx.changed()).await;
    }
}

#[tokio::test]
async fn bridge_reads_position_and_state_from_a_player() {
    let (addr, _player) = spawn_fake_player(fast_player(), 1).await;
    let (mut rx, _cmd) = spawn_bridge_client(addr.clone());

    let snap = wait_for(&mut rx, "the link to come up", |s| {
        s.link == LinkState::Connected && s.media.is_some()
    })
    .await;

    assert_eq!(snap.media.as_deref(), Some("C:\\VR\\fake-clip.mp4"));
    assert_eq!(snap.duration_s, Some(600.0));
    assert_eq!(snap.playing, Some(true));
    assert_eq!(snap.speed, Some(1.0));

    // Position must actually advance - a single opening packet is not proof
    // that the stream is being read.
    let first = snap.position_s.unwrap();
    let later = wait_for(&mut rx, "a stream of advancing positions", |s| {
        s.position_s.map(|p| p > first).unwrap_or(false) && s.packets > 3
    })
    .await;

    // Duration arrives only in full packets; it must survive the partial ones
    // in between. This is the merge-not-replace behaviour, proven on the wire
    // rather than in a unit test.
    assert_eq!(later.duration_s, Some(600.0));
}

/// The keepalive is the single most load-bearing detail of this protocol: the
/// player hangs up after 3 s of silence. The fake player enforces that, so
/// surviving past it is real evidence the heartbeat works.
#[tokio::test]
async fn link_survives_past_the_three_second_keepalive_timeout() {
    let (addr, _player) = spawn_fake_player(
        FakePlayerConfig {
            tick: Duration::from_millis(200),
            enforce_timeout: true,
            ..Default::default()
        },
        1,
    )
    .await;
    let (mut rx, _cmd) = spawn_bridge_client(addr);

    wait_for(&mut rx, "the link to come up", |s| {
        s.link == LinkState::Connected
    })
    .await;
    let packets_before = rx.borrow().packets;

    // Longer than the 3 s timeout, with margin.
    tokio::time::sleep(Duration::from_millis(4500)).await;

    let snap = rx.borrow_and_update().clone();
    assert_eq!(
        snap.link,
        LinkState::Connected,
        "player dropped us - the 1 Hz keepalive is not being sent"
    );
    assert!(
        snap.packets > packets_before,
        "still connected but no new packets - the read loop stalled"
    );
}

/// The player being absent is the normal state during development. The bridge
/// must sit there retrying, not exit and not spin.
#[tokio::test]
async fn absent_player_is_not_fatal() {
    // Bind and immediately drop, so the port is almost certainly free.
    let dead = {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().to_string()
    };

    let (mut rx, _cmd) = spawn_bridge_client(dead.clone());
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let snap = rx.borrow_and_update().clone();
    assert_ne!(snap.link, LinkState::Connected);
    assert_eq!(snap.position_s, None, "must not invent a position");
    assert_eq!(snap.endpoint, dead, "endpoint is still reported while down");
}

#[tokio::test]
async fn bridge_reconnects_after_the_player_goes_away() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();

    // Serve one client, then hang up on it, then serve another.
    let cfg = fast_player();
    tokio::spawn(async move {
        for round in 0..2 {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            if round == 0 {
                // Let the bridge see some state, then drop the socket
                // abruptly - the "headset went to sleep" case.
                let cfg = cfg.clone();
                let _ =
                    tokio::time::timeout(Duration::from_millis(400), serve_client(stream, &cfg, &Tap::disabled()))
                        .await;
            } else {
                serve_client(stream, &cfg, &Tap::disabled()).await;
            }
        }
    });

    let (mut rx, _cmd) = spawn_bridge_client(addr);

    wait_for(&mut rx, "the first connection", |s| {
        s.link == LinkState::Connected
    })
    .await;
    let dropped = wait_for(&mut rx, "the drop to be noticed", |s| {
        s.link != LinkState::Connected
    })
    .await;
    assert_eq!(
        dropped.position_s, None,
        "stale position must be cleared on drop"
    );
    assert_eq!(dropped.media, None);

    wait_for(&mut rx, "the reconnect", |s| {
        s.link == LinkState::Connected && s.media.is_some()
    })
    .await;
}

// ---------------------------------------------------------------------------
// The phone-facing half
// ---------------------------------------------------------------------------

/// Bring the whole thing up: fake player, bridge client, HTTP/WS server.
/// Returns the base `http://127.0.0.1:port`.
async fn spawn_full_stack(limit: usize) -> (String, mpsc::Sender<PlayerCommand>, Token) {
    let (player_addr, _player) = spawn_fake_player(fast_player(), limit).await;
    std::mem::forget(_player);

    let (snapshot_rx, cmd_tx) = spawn_bridge_client(player_addr);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");

    let token = Token::generate();
    let ctx = std::sync::Arc::new(http::Ctx {
        snapshot_rx,
        cmd_tx: cmd_tx.clone(),
        static_dir: None,
        pairing_url: coyote_bridge::auth::with_token(&base, &token),
        token: std::sync::RwLock::new(token.clone()),
        allowed_hosts: vec![format!("127.0.0.1:{port}")],
        on_token_rotated: None,
    });
    tokio::spawn(http::run(listener, ctx));

    (base, cmd_tx, token)
}

async fn get(url: &str) -> (u16, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let rest = url.strip_prefix("http://").unwrap();
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let mut stream = tokio::net::TcpStream::connect(host).await.unwrap();
    stream
        .write_all(
            format!("GET /{path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).await.unwrap();
    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body = raw
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    (status, body)
}

#[tokio::test]
async fn websocket_client_receives_a_hello_then_player_state() {
    let (base, _cmd, token) = spawn_full_stack(1).await;
    let ws_url = format!("{}/ws?t={token}", base.replace("http://", "ws://"));

    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();

    let hello: serde_json::Value = serde_json::from_str(&next_text(&mut ws).await).unwrap();
    assert_eq!(hello["type"], "hello");
    // The handshake must be explicit about what it carries, so a client author
    // does not have to guess from an empty first snapshot.
    let carries = hello["carries"].as_array().unwrap();
    for field in ["position", "playing", "duration", "media"] {
        assert!(carries.iter().any(|v| v == field), "hello omits {field}");
    }

    // Then snapshots. Keep reading until one arrives with real playback state.
    let deadline = tokio::time::Instant::now() + SETTLE;
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no populated snapshot arrived"
        );
        let msg: serde_json::Value = serde_json::from_str(&next_text(&mut ws).await).unwrap();
        assert_eq!(msg["type"], "player");
        if msg["link"] == "connected" && !msg["positionS"].is_null() {
            assert_eq!(msg["media"], "C:\\VR\\fake-clip.mp4");
            assert_eq!(msg["durationS"], 600.0);
            assert_eq!(msg["playing"], true);
            assert!(msg["updatedAtMs"].as_u64().unwrap() > 0);
            return;
        }
    }
}

#[tokio::test]
async fn websocket_seek_reaches_the_player_and_comes_back() {
    let (base, _cmd, token) = spawn_full_stack(1).await;
    let ws_url = format!("{}/ws?t={token}", base.replace("http://", "ws://"));
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();

    // Wait until the link is up before commanding it.
    let deadline = tokio::time::Instant::now() + SETTLE;
    loop {
        assert!(tokio::time::Instant::now() < deadline, "link never came up");
        let msg: serde_json::Value = serde_json::from_str(&next_text(&mut ws).await).unwrap();
        if msg["link"] == "connected" && !msg["positionS"].is_null() {
            break;
        }
    }

    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        r#"{"type":"seek","positionS":123.0}"#.into(),
    ))
    .await
    .unwrap();

    // The fake player should report a position at or just past the seek target.
    let deadline = tokio::time::Instant::now() + SETTLE;
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "seek never took effect"
        );
        let msg: serde_json::Value = serde_json::from_str(&next_text(&mut ws).await).unwrap();
        if let Some(p) = msg["positionS"].as_f64() {
            if (123.0..124.0).contains(&p) {
                return;
            }
        }
    }
}

#[tokio::test]
async fn built_in_pages_are_served() {
    let (base, _cmd, token) = spawn_full_stack(1).await;

    let (status, body) = get(&format!("{base}/healthz?t={token}")).await;
    assert_eq!(status, 200);
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["type"], "player");

    let (status, body) = get(&format!("{base}/qr.svg")).await;
    assert_eq!(status, 200);
    assert!(body.starts_with("<svg"), "qr.svg body was: {body:.80}");

    let (status, body) = get(&format!("{base}/pair")).await;
    assert_eq!(status, 200);
    assert!(body.contains("/qr.svg"));
    assert!(body.contains(&base), "pairing page should show the URL");

    // No static dir configured: the placeholder must say so rather than 404.
    let (status, body) = get(&format!("{base}/")).await;
    assert_eq!(status, 200);
    assert!(body.contains("--static-dir"));
}

#[tokio::test]
async fn traversal_outside_the_static_root_is_refused() {
    let dir = std::env::temp_dir().join(format!("coyote-bridge-test-{}", std::process::id()));
    tokio::fs::create_dir_all(&dir).await.unwrap();
    tokio::fs::write(dir.join("index.html"), "<h1>app</h1>")
        .await
        .unwrap();

    let (snapshot_rx, cmd_tx) = spawn_bridge_client("127.0.0.1:1".into());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");
    tokio::spawn(http::run(
        listener,
        std::sync::Arc::new(http::Ctx {
            snapshot_rx,
            cmd_tx,
            static_dir: Some(dir.clone()),
            pairing_url: base.clone(),
            token: std::sync::RwLock::new(Token::generate()),
            allowed_hosts: vec![format!("127.0.0.1:{port}")],
            on_token_rotated: None,
        }),
    ));

    let (status, body) = get(&format!("{base}/")).await;
    assert_eq!(status, 200);
    assert!(body.contains("app"));

    let (status, _) = get(&format!("{base}/../../../Windows/win.ini")).await;
    assert_eq!(status, 403, "traversal must be refused, not served");

    let _ = tokio::fs::remove_dir_all(&dir).await;
}

/// The client against **recorded traffic from a real DeoVR on a Quest**.
///
/// Every other test in this file runs the client against a fake written from
/// the same reading of the same documents as the client itself, so a shared
/// misreading passes them all. This one does not have that problem: the bytes
/// came off a real headset. It is the only test here whose failure would mean
/// "we broke compatibility with a real player" rather than "we broke
/// compatibility with our own assumptions".
///
/// Scope, precisely: one player, one version, one platform, three frames of
/// steady-state playback. See `capture.rs`.
#[tokio::test]
async fn client_reads_recorded_traffic_from_a_real_deovr() {
    let (addr, _player) = spawn_fake_player(FakePlayerConfig::replay_deovr(), 1).await;
    let (mut rx, _cmd) = spawn_bridge_client(addr);

    let snap = wait_for(&mut rx, "the recorded session to be read", |s| {
        s.link == LinkState::Connected && s.packets >= 2
    })
    .await;

    // Values taken from the capture, not from anything we invented.
    assert_eq!(snap.duration_s, Some(7693.12));
    assert!(
        snap.media
            .as_deref()
            .unwrap_or_default()
            .starts_with("http://"),
        "a real DeoVR reports a URL, not a filesystem path: {:?}",
        snap.media
    );
    assert_eq!(snap.speed, Some(1.0));
    assert_eq!(snap.epoch, 1, "the first connection is epoch 1");

    let start = snap.position_s.expect("position must be known");
    let advanced = wait_for(&mut rx, "position to advance", |s| {
        s.position_s.unwrap_or(0.0) > start + 1.0
    })
    .await;
    assert!(advanced.position_s.unwrap() > start);

    // The `playerState` contradiction is not asserted here: it appears 76
    // seconds into the recording, and this test replays in real time. It is
    // covered in `capture::tests`, which walks the whole connection in memory.
}

/// The attack the token exists to stop, run against the real server.
///
/// A page the user visits can reach a LAN address, and WebSockets are not
/// subject to the same-origin policy — so without a token, any tab could open
/// this socket and issue `seek`, `play` and `pause`. These assert that it
/// cannot.
#[tokio::test]
async fn an_untokened_request_is_refused() {
    let (base, _cmd, _token) = spawn_full_stack(1).await;

    let (status, body) = get(&format!("{base}/healthz")).await;
    assert_eq!(status, 401, "healthz leaks the media URL and LAN address");
    assert!(!body.contains("positionS"));

    let (status, _) = get(&format!("{base}/healthz?t=wrong")).await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn a_websocket_without_a_token_cannot_open() {
    let (base, _cmd, _token) = spawn_full_stack(1).await;
    let ws_url = base.replace("http://", "ws://") + "/ws";
    assert!(
        tokio_tungstenite::connect_async(&ws_url).await.is_err(),
        "the socket that can drive the player must not open unauthenticated"
    );
}

/// The pairing page has to stay reachable: it is how a phone *obtains* the
/// token, so gating it would be a bootstrap that cannot start.
#[tokio::test]
async fn the_pairing_page_stays_reachable_without_a_token() {
    let (base, _cmd, _token) = spawn_full_stack(1).await;
    let (status, body) = get(&format!("{base}/pair")).await;
    assert_eq!(status, 200);
    assert!(body.contains("qr.svg"));
}

async fn next_text<S>(ws: &mut S) -> String
where
    S: StreamExt<
            Item = Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Unpin,
{
    loop {
        match tokio::time::timeout(SETTLE, ws.next()).await {
            Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Text(t)))) => return t,
            Ok(Some(Ok(_))) => continue,
            other => panic!("websocket ended unexpectedly: {other:?}"),
        }
    }
}
