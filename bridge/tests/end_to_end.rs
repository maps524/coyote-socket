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
        library: None,
        pairing_base: base.clone(),
        token: std::sync::RwLock::new(token.clone()),
        allowed_hosts: vec![format!("127.0.0.1:{port}")],
        on_token_rotated: None,
        // Plain HTTP: these tests cover routing and authorization, not the
        // secure context. `/install` correctly reports nothing to install.
        tls: None,
        devices: test_device_store(),
        clients: Default::default(),
    });
    tokio::spawn(http::run(listener, ctx));

    (base, cmd_tx, token)
}

#[tokio::test]
async fn pairing_is_refused_over_plaintext() {
    // Issuing a long-lived credential in cleartext would hand it to the same
    // eavesdropper the pairing token was already exposed to — and make that
    // exposure permanent, since the cookie outlives the pairing moment. The
    // token's exposure is a window; a credential's would be forever.
    let (base, _cmd_tx, token) = spawn_full_stack(64).await;
    let (status, _body) = get(&format!("{base}/pair/exchange?t={token}")).await;
    assert_eq!(
        status, 403,
        "pairing must require a secure transport, exactly as rotation does"
    );
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
    // `library` is advertised whether or not one is configured, so a client can
    // tell a bridge that has the endpoints from one too old to have them —
    // which it otherwise cannot, because an old bridge answers
    // `/library/index.json` with the SPA fallback's `index.html` and a 200.
    for field in ["position", "playing", "duration", "media", "library"] {
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
        // The stream carries two types now. A consumer must dispatch on `type`
        // rather than assuming everything after the hello is a snapshot.
        if msg["type"] == "library" {
            continue;
        }
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
            library: None,
            pairing_base: base.clone(),
            token: std::sync::RwLock::new(Token::generate()),
            allowed_hosts: vec![format!("127.0.0.1:{port}")],
            on_token_rotated: None,
            tls: None,
        devices: test_device_store(),
        clients: Default::default(),
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

/// **The token must never reach the log.**
///
/// The log file, the ring buffer, stderr and the desktop window's log pane are
/// one stream, and the window has a "Copy all" button whose stated purpose is
/// assembling a block of text to paste into a bug report. A request line logged
/// with its query string put a persistent, password-equivalent credential into
/// exactly the thing users are encouraged to share.
///
/// This asserts the property rather than the fix, so a future call site that
/// logs a URL fails here rather than shipping — **but only over the routes it
/// actually drives.** A token-carrying route added later and not added here is
/// covered by the docstring's claim and not by the test, which is worse than an
/// obviously narrow test. Every route that takes a token belongs in the list
/// below.
#[tokio::test]
async fn the_token_never_appears_in_the_log() {
    coyote_bridge::logging::init(Some(std::env::temp_dir().join("coyote-bridge-log-test")));
    let (base, _cmd, token) = spawn_full_stack(1).await;

    // Exercise every route that takes a token, plus a refusal.
    let _ = get(&format!("{base}/healthz?t={token}")).await;
    let _ = get(&format!("{base}/healthz?t=wrong")).await;
    let _ = get(&format!("{base}/pair")).await;
    let _ = get(&format!("{base}/pair/rotate?t={token}")).await;
    // Refused here because it is plaintext, but the request line is logged
    // before the transport is checked — which is exactly the interesting case:
    // a route can leak the token without ever succeeding.
    let _ = get(&format!("{base}/pair/exchange?t={token}")).await;
    let _ = get(&format!("{base}/install?t={token}")).await;

    let history = coyote_bridge::logging::history().join("\n");
    assert!(
        !history.contains(token.as_str()),
        "the pairing token reached the log:\n{history}"
    );
    // And the redaction should be visible rather than silent, so a reader can
    // tell "no query" from "query withheld".
    assert!(
        history.contains("<redacted>"),
        "expected a redaction marker in:\n{history}"
    );
}

/// **The keepalive contract, end to end.**
///
/// A consumer that stops driving hardware on silence needs silence to mean
/// exactly one thing. This asserts it against a player that is not merely
/// paused but entirely absent — the harshest case, since nothing at all is
/// arriving from the player side to accidentally keep the stream warm.
///
/// The dependency this protects is real and downstream: the PWA distinguishes
/// "player paused" (hold the clock, keep driving output) from "bridge silent"
/// (stop). Confusing those in the wrong direction means output continues when
/// nothing is watching.
#[tokio::test]
async fn the_relay_keeps_talking_while_the_player_says_nothing() {
    // No player task at all, and a snapshot nobody ever touches. Pointing the
    // client at a dead port would not do: its retry loop flips the link
    // between Connecting and Retrying on a backoff, and those changes would
    // drive the relay on their own — the test would pass without a keepalive
    // existing. The guarantee is about a stream with *no* state changes in it.
    let (snapshot_tx, snapshot_rx) = watch::channel(PlayerSnapshot::new("nobody:23554".into()));
    let (cmd_tx, _cmd_rx) = mpsc::channel(4);
    std::mem::forget(snapshot_tx); // keep the channel open, change nothing

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");
    let token = Token::generate();
    tokio::spawn(http::run(
        listener,
        std::sync::Arc::new(http::Ctx {
            snapshot_rx,
            cmd_tx,
            static_dir: None,
            library: None,
            pairing_base: base.clone(),
            token: std::sync::RwLock::new(token.clone()),
            allowed_hosts: vec![format!("127.0.0.1:{port}")],
            on_token_rotated: None,
            // Plain HTTP: these tests cover routing and authorization, not
            // the secure context. `/install` correctly reports nothing to install.
            tls: None,
        devices: test_device_store(),
        clients: Default::default(),
        }),
    ));

    let ws_url = format!("{}/ws?t={token}", base.replace("http://", "ws://"));
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();

    let _hello = next_text(&mut ws).await;
    let _initial = next_text(&mut ws).await;

    // Three keepalives, each inside a window generous enough to survive a
    // loaded CI box but far short of any plausible safety deadline.
    let budget = http::RELAY_KEEPALIVE * 4;
    for round in 1..=3 {
        let text = tokio::time::timeout(budget, next_text(&mut ws))
            .await
            .unwrap_or_else(|_| {
                panic!("no keepalive within {budget:?} on round {round} — a consumer would \
                        have concluded the bridge was dead while it was fine")
            });
        let msg: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(msg["type"], "player", "keepalives carry the normal snapshot");
        // And it says the player is absent, rather than leaving that to be
        // inferred from silence.
        assert_ne!(msg["link"], "connected");
    }
}

/// A *paused* player still reaches the phone, on its own merits.
///
/// Distinct from the keepalive test above, and it passes without the keepalive
/// existing — deliberately so. A paused DeoVR keeps sending `currentTime`
/// unchanged at its normal cadence, and this asserts that a snapshot whose
/// playback fields are identical still gets pushed. If `watch` suppressed a
/// no-op update, a paused player would fall back on the keepalive and be
/// indistinguishable from a stopped one at the exact resolution a consumer
/// cares about.
///
/// This is the case the downstream design turns on: "paused" means hold the
/// clock and keep driving output; "silent" means stop. Confusing them in the
/// wrong direction means output continues when nothing is watching.
#[tokio::test]
async fn a_paused_player_keeps_the_relay_talking() {
    let (snapshot_tx, snapshot_rx) = watch::channel(PlayerSnapshot::new("paused:23554".into()));
    let (cmd_tx, _cmd_rx) = mpsc::channel(4);

    // A player that is connected and paused: position never moves.
    snapshot_tx.send_modify(|s| {
        s.link = LinkState::Connected;
        s.epoch = 1;
        let packet: coyote_bridge::state::PlayerPacket =
            serde_json::from_str(r#"{"path":"a.mp4","duration":600.0,"currentTime":42.0,"playerState":1}"#)
                .unwrap();
        s.apply(&packet, 1_000);
    });

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");
    let token = Token::generate();
    tokio::spawn(http::run(
        listener,
        std::sync::Arc::new(http::Ctx {
            snapshot_rx,
            cmd_tx,
            static_dir: None,
            library: None,
            pairing_base: base.clone(),
            token: std::sync::RwLock::new(token.clone()),
            allowed_hosts: vec![format!("127.0.0.1:{port}")],
            on_token_rotated: None,
            // Plain HTTP: these tests cover routing and authorization, not
            // the secure context. `/install` correctly reports nothing to install.
            tls: None,
        devices: test_device_store(),
        clients: Default::default(),
        }),
    ));

    let ws_url = format!("{}/ws?t={token}", base.replace("http://", "ws://"));
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();
    let _hello = next_text(&mut ws).await;
    let _initial = next_text(&mut ws).await;

    // Repeat the identical packet at the player's real cadence, as a paused
    // DeoVR does, and require the relay to keep pushing.
    let packet: coyote_bridge::state::PlayerPacket =
        serde_json::from_str(r#"{"currentTime":42.0,"playerState":1}"#).unwrap();
    let budget = http::RELAY_KEEPALIVE * 4;
    for round in 1..=3u64 {
        snapshot_tx.send_modify(|s| s.apply(&packet, 1_000 + round * 1_000));
        let text = tokio::time::timeout(budget, next_text(&mut ws))
            .await
            .unwrap_or_else(|_| panic!("paused player produced no message on round {round}"));
        let msg: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(msg["link"], "connected", "still connected, just paused");
        assert_eq!(msg["positionS"], 42.0, "position is unchanged, as expected");
    }
}

/// Rotation is refused over plaintext. A replacement token delivered in
/// cleartext hands the eavesdropper the replacement too.
#[tokio::test]
async fn rotation_is_refused_over_plain_http() {
    let (base, _cmd, token) = spawn_full_stack(1).await;
    let (status, body) = get(&format!("{base}/pair/rotate?t={token}")).await;
    assert_eq!(status, 403);
    assert!(body.contains("secure transport"), "got: {body}");

    // And the token still works, so a refused rotation is not a silent
    // half-rotation.
    let (status, _) = get(&format!("{base}/healthz?t={token}")).await;
    assert_eq!(status, 200);
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

// ---------------------------------------------------------------------------
// The funscript library
// ---------------------------------------------------------------------------

/// A bridge serving a real directory of funscripts on a real socket.
async fn spawn_library_stack(root: std::path::PathBuf) -> (String, Token) {
    let (snapshot_rx, cmd_tx) = spawn_bridge_client("127.0.0.1:1".into());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");
    let token = Token::generate();
    tokio::spawn(http::run(
        listener,
        std::sync::Arc::new(http::Ctx {
            snapshot_rx,
            cmd_tx,
            static_dir: None,
            library: Some(coyote_bridge::library::Library::spawn(root)),
            pairing_base: base.clone(),
            token: std::sync::RwLock::new(token.clone()),
            allowed_hosts: vec![format!("127.0.0.1:{port}")],
            on_token_rotated: None,
            tls: None,
            devices: test_device_store(),
        clients: Default::default(),
        }),
    ));
    (base, token)
}

fn library_root(name: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("coyote-library-e2e-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The whole point of the feature, over the wire: a phone asks what is on the
/// machine and then asks for one of them, without importing anything by hand.
#[tokio::test]
async fn the_library_index_and_a_script_are_served_over_http() {
    let root = library_root("serve");
    // A space in the name, because funscript libraries are full of them and a
    // server that does not percent-decode cannot serve this file at all.
    std::fs::write(root.join("Scene One.funscript"), r#"{"actions":[]}"#).unwrap();
    std::fs::write(root.join("Scene One.roll.funscript"), r#"{"actions":[1]}"#).unwrap();
    std::fs::write(root.join("notes.txt"), "not a script").unwrap();

    let (base, token) = spawn_library_stack(root.clone()).await;

    // The listing is gated, like `/healthz`: it names every file in a
    // directory the user chose.
    let (status, _) = get(&format!("{base}/library/index.json")).await;
    assert_eq!(
        status, 401,
        "the listing must not be readable without a token"
    );

    // The first scan may not have landed on the very first request.
    let deadline = tokio::time::Instant::now() + SETTLE;
    let index = loop {
        assert!(tokio::time::Instant::now() < deadline, "no index arrived");
        let (status, body) = get(&format!("{base}/library/index.json?t={token}")).await;
        assert_eq!(status, 200);
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        if json["scripts"].as_array().unwrap().len() == 2 {
            break json;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };

    assert_eq!(index["configured"], true);
    // Sorted, so the order a client renders does not shuffle between scans.
    assert_eq!(index["scripts"][0]["name"], "Scene One.funscript");
    assert_eq!(index["scripts"][1]["name"], "Scene One.roll.funscript");
    assert_eq!(index["scripts"][0]["bytes"], 14);
    assert!(index["scripts"][0]["modifiedMs"].as_u64().unwrap() > 0);
    // The index is a snapshot and says so, rather than letting a consumer
    // assume it is live.
    assert!(index["scannedAtMs"].as_u64().unwrap() > 0);
    assert!(index["ageMs"].is_number());
    assert!(index["generation"].as_u64().unwrap() >= 1);

    // And the bytes, by the name the index published.
    let (status, body) = get(&format!("{base}/library/Scene%20One.funscript?t={token}")).await;
    assert_eq!(status, 200, "a name with a space must be servable");
    assert_eq!(body, r#"{"actions":[]}"#);

    let (status, _) = get(&format!("{base}/library/Scene%20One.funscript")).await;
    assert_eq!(status, 401, "the bytes are gated too");

    let _ = std::fs::remove_dir_all(&root);
}

/// What a hostile `name` can reach: nothing.
///
/// The encoded forms matter most. Decoding happens before validation, so `..`
/// is visible to the path check rather than hidden behind `%2e%2e` — the
/// opposite order is the classic way a decoder reintroduces traversal into a
/// server that had already rejected it.
#[tokio::test]
async fn a_hostile_library_name_reaches_nothing() {
    let root = library_root("hostile");
    std::fs::write(root.join("ok.funscript"), "{}").unwrap();
    std::fs::write(root.join("notes.txt"), "not a script").unwrap();
    // A file the request must not be able to reach: a sibling of the library
    // root, with an extension that would otherwise pass the type gate.
    let secret = root
        .parent()
        .unwrap()
        .join("coyote-library-secret.funscript");
    std::fs::write(&secret, "SECRET").unwrap();

    let (base, token) = spawn_library_stack(root.clone()).await;

    for (name, expected) in [
        // Encoded traversal, both separators and both cases.
        ("%2e%2e%2fcoyote-library-secret.funscript", 403),
        ("%2E%2E%5Ccoyote-library-secret.funscript", 403),
        ("..%2fcoyote-library-secret.funscript", 403),
        ("..%5Ccoyote-library-secret.funscript", 403),
        // Unencoded, for completeness — the request line carries them fine.
        ("../coyote-library-secret.funscript", 403),
        // Absolute and drive-qualified.
        ("%2fetc%2fpasswd", 403),
        ("C%3A%5CWindows%5Cwin.ini", 403),
        // A malformed escape is a rejection, not a literal percent.
        ("ok%2.funscript", 403),
        // A decoded NUL, which truncates a path in the C APIs under `std`.
        ("ok%00.funscript", 403),
        // Right directory, wrong type: only funscripts are reachable.
        ("notes.txt", 403),
        // Well-formed and inside the root, but not something the scan listed.
        ("never-existed.funscript", 404),
    ] {
        let (status, body) = get(&format!("{base}/library/{name}?t={token}")).await;
        assert_eq!(status, expected, "{name} returned {status}: {body:.60}");
        assert!(!body.contains("SECRET"), "{name} reached the secret file");
    }

    // And the legitimate name still works, so the gates are not simply
    // refusing everything.
    let deadline = tokio::time::Instant::now() + SETTLE;
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "ok.funscript never became servable"
        );
        let (status, body) = get(&format!("{base}/library/ok.funscript?t={token}")).await;
        if status == 200 {
            assert_eq!(body, "{}");
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let _ = std::fs::remove_file(&secret);
    let _ = std::fs::remove_dir_all(&root);
}

/// A phone that is already connected picks up a new file without a reload.
///
/// The message carries no contents by design — only "ask again" — so this
/// asserts the wake and the generation, not a listing.
#[tokio::test]
async fn a_new_file_reaches_an_already_connected_client() {
    let root = library_root("push");
    std::fs::write(root.join("first.funscript"), "{}").unwrap();

    let (base, token) = spawn_library_stack(root.clone()).await;
    let ws_url = format!("{}/ws?t={token}", base.replace("http://", "ws://"));
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();

    let hello: serde_json::Value = serde_json::from_str(&next_text(&mut ws).await).unwrap();
    assert_eq!(hello["type"], "hello");

    // One `library` message up front, so "fetch on every library message" is
    // the client's only rule. It may arrive before the first scan has landed —
    // the socket does not wait on the disk — so settle on the one that has seen
    // the file that was there when the test started.
    let first = wait_for_library(&mut ws, |m| m["count"] == 1).await;
    let first_gen = first["generation"].as_u64().unwrap();
    assert!(
        first.get("scripts").is_none(),
        "the message must not carry the listing"
    );

    std::fs::write(root.join("second.funscript"), "{}").unwrap();

    let next = wait_for_library(&mut ws, |m| m["count"] == 2).await;
    assert_eq!(
        next["generation"].as_u64().unwrap(),
        first_gen + 1,
        "a new file must bump the generation exactly once"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// Read messages until a matching `library` one arrives, skipping the player
/// snapshots that keep flowing alongside it.
async fn wait_for_library<S>(
    ws: &mut S,
    matches: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value
where
    S: StreamExt<
            Item = Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Unpin,
{
    let deadline = tokio::time::Instant::now() + SETTLE;
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no library message arrived"
        );
        let msg: serde_json::Value = serde_json::from_str(&next_text(ws).await).unwrap();
        if msg["type"] == "library" && matches(&msg) {
            return msg;
        }
    }
}

/// No library configured is a normal state: an empty listing and a flag that
/// lets the UI say "you have not pointed me at a folder" rather than showing a
/// failure.
#[tokio::test]
async fn no_library_configured_is_not_an_error() {
    let (base, _cmd, token) = spawn_full_stack(1).await;

    let (status, body) = get(&format!("{base}/library/index.json?t={token}")).await;
    assert_eq!(status, 200, "an unconfigured library is not a 404");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["configured"], false);
    assert_eq!(json["scripts"].as_array().unwrap().len(), 0);
    // Not "taken in 1970". A snapshot that was never taken has no age.
    assert!(json["ageMs"].is_null());
    assert!(json["scannedAtMs"].is_null());

    let (status, _) = get(&format!("{base}/library/anything.funscript?t={token}")).await;
    assert_eq!(status, 404);
}

/// A share that drops must not publish "your library is empty" to every phone.
///
/// The end-to-end half of `library::tests`' unit coverage: what the *response*
/// says after the root vanishes, since that is what a client actually reads.
#[tokio::test]
async fn a_vanished_library_root_does_not_report_an_empty_library() {
    let root = library_root("vanish");
    std::fs::write(root.join("only.funscript"), "{}").unwrap();

    let (base, token) = spawn_library_stack(root.clone()).await;

    let deadline = tokio::time::Instant::now() + SETTLE;
    loop {
        assert!(tokio::time::Instant::now() < deadline, "no scan landed");
        let (_, body) = get(&format!("{base}/library/index.json?t={token}")).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        if json["scan"] == "ok" {
            assert_eq!(json["scripts"].as_array().unwrap().len(), 1);
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    std::fs::remove_dir_all(&root).unwrap();

    let deadline = tokio::time::Instant::now() + SETTLE;
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the failure was never reported"
        );
        let (status, body) = get(&format!("{base}/library/index.json?t={token}")).await;
        assert_eq!(status, 200);
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        if json["scan"] == "failed" {
            assert_eq!(
                json["scripts"].as_array().unwrap().len(),
                1,
                "a failed scan established nothing and must not empty the listing"
            );
            assert_eq!(json["configured"], true);
            // The listing's age keeps describing the listing, and `checkedAtMs`
            // is the field that moves — so a client can see how far behind it
            // has fallen.
            assert!(json["ageMs"].as_u64().unwrap() > 0);
            assert!(json["checkedAtMs"].as_u64().unwrap() > json["scannedAtMs"].as_u64().unwrap());
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// A file over the cap is left out of the listing, so a client never renders a
/// script it could only ever fail to load.
///
/// It is therefore unreachable as a 404 rather than a 413 — the 413 survives
/// only for a file that grows *after* it was indexed, and its reason phrase is
/// asserted in `http`'s own tests.
#[tokio::test]
async fn an_oversized_script_is_not_advertised() {
    let root = library_root("oversize");
    std::fs::write(root.join("fine.funscript"), "{}").unwrap();
    // `set_len` on a fresh file is instant — no 64 MB is written.
    let big = std::fs::File::create(root.join("huge.funscript")).unwrap();
    big.set_len(coyote_bridge::library::MAX_SCRIPT_BYTES + 1)
        .unwrap();
    drop(big);

    let (base, token) = spawn_library_stack(root.clone()).await;

    let deadline = tokio::time::Instant::now() + SETTLE;
    loop {
        assert!(tokio::time::Instant::now() < deadline, "no scan landed");
        let (_, body) = get(&format!("{base}/library/index.json?t={token}")).await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        if json["scan"] == "ok" {
            let names: Vec<_> = json["scripts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| s["name"].as_str().unwrap().to_string())
                .collect();
            assert_eq!(
                names,
                ["fine.funscript"],
                "an oversized file must not be advertised as playable"
            );
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Unlisted means unfetchable, by the index-membership check.
    let (status, _) = get(&format!("{base}/library/huge.funscript?t={token}")).await;
    assert_eq!(status, 404);

    let _ = std::fs::remove_dir_all(&root);
}

/// A credential store in a scratch file, unique per process and per call.
///
/// Never the real one: these tests must not be able to pair a device into a
/// developer's actual bridge, and two tests running in parallel must not fight
/// over one file.
fn test_device_store() -> std::sync::Arc<coyote_bridge::devices::DeviceStore> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "coyote-bridge-test-devices-{}-{n}.json",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    std::sync::Arc::new(coyote_bridge::devices::DeviceStore::load(path))
}

// ---------------------------------------------------------------------------
// Who is connected
// ---------------------------------------------------------------------------
//
// The unit tests in `clients.rs` cover the counting rules. These check the two
// things only the real surface can prove: that a live WebSocket actually
// registers, and that `/healthz` carries the answer — which is the headless
// build's only way to see it.

/// A bridge whose registry and snapshot channel the test keeps hold of.
///
/// `spawn_full_stack` hands both to a fake player and keeps neither, which is
/// right for the tests that came before. These need to install a credential
/// resolver and to drive the snapshot channel directly.
struct Stack {
    base: String,
    token: Token,
    clients: std::sync::Arc<coyote_bridge::clients::ClientRegistry>,
    snapshots: watch::Sender<PlayerSnapshot>,
}

async fn spawn_stack(
    resolver: Option<coyote_bridge::clients::CredentialResolver>,
) -> Stack {
    let (snapshot_tx, snapshot_rx) = watch::channel(PlayerSnapshot::new("127.0.0.1:23554".into()));
    let (cmd_tx, _cmd_rx) = mpsc::channel(16);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");
    let token = Token::generate();

    let clients = std::sync::Arc::new(coyote_bridge::clients::ClientRegistry::new());
    if let Some(resolver) = resolver {
        clients.set_credential_resolver(resolver);
    }

    tokio::spawn(http::run(
        listener,
        std::sync::Arc::new(http::Ctx {
            snapshot_rx,
            cmd_tx,
            static_dir: None,
            pairing_base: base.clone(),
            token: std::sync::RwLock::new(token.clone()),
            allowed_hosts: vec![format!("127.0.0.1:{port}")],
            on_token_rotated: None,
            library: None,
            tls: None,
            devices: test_device_store(),
            clients: std::sync::Arc::clone(&clients),
        }),
    ));

    Stack {
        base,
        token,
        clients,
        snapshots: snapshot_tx,
    }
}

/// The contract of the identity seam, driven over a real HTTP request.
///
/// **This test is the seam.** Everything else about per-device credentials is a
/// type signature and an agreement in a message; without a resolver installed
/// and a real `Cookie` header on a real upgrade, "the credential decides who
/// this is" is asserted rather than demonstrated. It also pins the shape
/// `bridge-tls` must satisfy: the resolver is handed the **cookie header and
/// nothing else**, so a credential arriving as a query parameter or a
/// `Sec-WebSocket-Protocol` value would need this signature changed.
#[tokio::test]
async fn a_verified_credential_decides_who_a_client_is() {
    use coyote_bridge::clients::Credential;

    let stack = spawn_stack(Some(std::sync::Arc::new(|cookie: Option<&str>| {
        // Stands in for `DeviceStore::verify`, which does the same job with a
        // hashed secret behind it.
        cookie
            .is_some_and(|c| c.contains("coyote_device=known-device-secret"))
            .then(|| Credential {
                id: "dev-abc123".into(),
                label: Some("Sara's phone".into()),
                created_ms: Some(1_700_000_000_000),
            })
    })))
    .await;
    let (base, token) = (stack.base.clone(), stack.token.clone());

    // The client also volunteers an id. The credential must win: one of the two
    // was checked.
    let ws_url = format!(
        "{}/ws?t={token}&c=self-chosen-id",
        base.replace("http://", "ws://")
    );
    let request = ws_request_with_cookie(&ws_url, &base, "coyote_device=known-device-secret");
    let (mut ws, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let _ = next_text(&mut ws).await;

    let view = clients_until(&base, &token, |v| v["connections"] == 1).await;
    let row = &view["clients"][0];
    assert_eq!(row["provenance"], "credential");
    assert_eq!(row["id"], "dev-abc123", "not the id the client chose");
    assert_eq!(row["label"], "Sara's phone");
    assert_eq!(row["createdMs"], 1_700_000_000_000u64);
    assert_eq!(row["revocable"], true);
    assert_eq!(view["credentialsAvailable"], true);
    assert_eq!(view["browsers"]["state"], "reported");
    assert_eq!(view["browsers"]["provenance"], "credential");
}

/// An unrecognised cookie is not an identity. The resolver refusing must not
/// silently fall through to trusting whatever the client said instead — that
/// would make a forged cookie a *downgrade* to self-reported rather than a
/// refusal, which is a distinction worth having a test for.
#[tokio::test]
async fn an_unrecognised_cookie_falls_back_to_self_reported_and_says_so() {
    let stack = spawn_stack(Some(std::sync::Arc::new(|_: Option<&str>| None))).await;
    let (base, token) = (stack.base.clone(), stack.token.clone());

    let ws_url = format!(
        "{}/ws?t={token}&c=self-chosen-id",
        base.replace("http://", "ws://")
    );
    let request = ws_request_with_cookie(&ws_url, &base, "coyote_device=forged");
    let (mut ws, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let _ = next_text(&mut ws).await;

    let view = clients_until(&base, &token, |v| v["connections"] == 1).await;
    assert_eq!(view["clients"][0]["provenance"], "selfReported");
    assert_eq!(view["clients"][0]["revocable"], false);
    // The count must not read as though these were verified devices.
    assert_eq!(view["browsers"]["provenance"], "selfReported");
}

/// Revocation must beat a busy relay.
///
/// `tokio::select!` polls its arms in a **random** order unless the block opens
/// with `biased;`. Both the revoke signal and a pending snapshot are ready at
/// once here, so without bias the relay wins the coin toss about half the time
/// and sends state to a socket the user has just revoked.
///
/// # Why it is staged rather than simply flooding
///
/// The obvious version — flood the snapshot channel, revoke, count what
/// arrives — measures the wrong thing, and did: it counted **996** frames that
/// the relay had generated *before* the revoke and left sitting in the client's
/// receive buffer while the test slept. Backlog is not a race. So the channel
/// is held quiet until the relay is parked at the `select!`, and only then are
/// both arms made ready in the same instant.
///
/// Biased, the count is deterministically zero. Unbiased it is not: measured
/// against a build with `biased` removed, the relay leaks state frames on
/// roughly one round in five. Thirty rounds puts a false pass near one in a
/// thousand — the round count is calibration, not superstition, and reducing it
/// weakens the test rather than speeding it up.
#[tokio::test]
async fn a_revoked_socket_closes_before_the_relay_sends_more_state() {
    use coyote_bridge::clients::Credential;
    use std::sync::atomic::{AtomicBool, Ordering};

    let stack = spawn_stack(Some(std::sync::Arc::new(|cookie: Option<&str>| {
        cookie.is_some_and(|c| c.contains("dev=1")).then(|| Credential {
            id: "dev-revoke".into(),
            label: None,
            created_ms: None,
        })
    })))
    .await;
    let ws_url = format!(
        "{}/ws?t={}",
        stack.base.replace("http://", "ws://"),
        stack.token
    );

    // Quiet until released, so the relay is parked with nothing ready.
    let flooding = std::sync::Arc::new(AtomicBool::new(false));
    let snapshots = stack.snapshots.clone();
    let gate = std::sync::Arc::clone(&flooding);
    let spam = tokio::spawn(async move {
        loop {
            if gate.load(Ordering::Relaxed) {
                snapshots.send_modify(|s| s.packets += 1);
            }
            tokio::task::yield_now().await;
        }
    });

    let mut frames_after_revoke = 0usize;
    for _ in 0..30 {
        let request = ws_request_with_cookie(&ws_url, &stack.base, "dev=1");
        let (mut ws, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        // Drain the startup burst by *waiting for silence* rather than by
        // counting messages. Counting was wrong within a day: the library work
        // added a third opening frame, and a test that expected two then
        // attributed the third to the race it was measuring — reporting
        // exactly one leaked frame per round, deterministically, which looks
        // nothing like the random loss it was built to catch.
        loop {
            match tokio::time::timeout(Duration::from_millis(150), ws.next()).await {
                Ok(Some(Ok(_))) => continue,
                Err(_) => break, // silence: the relay is parked at the select
                other => panic!("websocket ended during startup: {other:?}"),
            }
        }

        // Both arms become ready together: the revoke signal, and a snapshot
        // channel that will not stop changing.
        assert_eq!(stack.clients.revoke("dev-revoke"), 1);
        flooding.store(true, Ordering::Relaxed);

        loop {
            match tokio::time::timeout(SETTLE, ws.next()).await {
                Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Text(_)))) => {
                    frames_after_revoke += 1;
                }
                Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Close(frame)))) => {
                    let frame = frame.expect("the close must say why");
                    assert_eq!(u16::from(frame.code), coyote_bridge::http::WS_CLOSE_REVOKED);
                    assert_eq!(frame.reason, "revoked");
                    break;
                }
                Ok(Some(Ok(_))) => continue,
                other => panic!("expected a close frame, got {other:?}"),
            }
        }
        flooding.store(false, Ordering::Relaxed);
    }
    spam.abort();

    assert_eq!(
        frames_after_revoke, 0,
        "a revoked socket received {frames_after_revoke} more state frames across \
         thirty rounds; `biased` is missing from the relay's select"
    );
}

/// Build a WebSocket upgrade carrying a `Cookie` header, as a browser sends it.
///
/// `connect_async` takes a URL and sends neither cookies nor an `Origin`; a
/// browser sends both, and the distinction is load-bearing. `authorised`
/// refuses any request that presents a cookie without an `Origin`, because
/// `SameSite=Lax` attaches the cookie to cross-site top-level navigations and
/// those carry no `Origin` — so accepting them would let a drive-by page act
/// with the credential.
///
/// A test that omitted the header would therefore be refused, and reading that
/// 401 as "my credential is wrong" rather than "my test is not a browser" is
/// exactly the misattribution this suite exists to prevent.
fn ws_request_with_cookie(
    url: &str,
    origin: &str,
    cookie: &str,
) -> tokio_tungstenite::tungstenite::handshake::client::Request {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut request = url.into_client_request().expect("a valid ws url");
    request
        .headers_mut()
        .insert("Cookie", cookie.parse().expect("a valid cookie header"));
    request
        .headers_mut()
        .insert("Origin", origin.parse().expect("a valid origin"));
    request
}

/// Poll `/healthz` until the client view satisfies `done`, or give up.
///
/// The relay reads a client message between snapshots, so identification is
/// not synchronous with the send that caused it.
async fn clients_until(
    base: &str,
    token: &Token,
    done: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let mut last = serde_json::Value::Null;
    for _ in 0..60 {
        let (_, body) = get(&format!("{base}/healthz?t={token}")).await;
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        last = v["clients"].clone();
        if done(&last) {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the client view never settled; last was {last}");
}

/// A connected client is visible in `/healthz`, and the snapshot's own keys are
/// untouched beside it. The addition has to be additive: a consumer reading
/// `positionS` off the top level predates this field.
#[tokio::test]
async fn healthz_reports_a_connected_client_without_disturbing_the_snapshot() {
    let (base, _cmd, token) = spawn_full_stack(1).await;

    let (_, body) = get(&format!("{base}/healthz?t={token}")).await;
    let before: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(before["clients"]["connections"], 0);
    assert_eq!(before["clients"]["browsers"]["state"], "reported");
    assert_eq!(before["clients"]["browsers"]["count"], 0);

    let ws_url = format!(
        "{}/ws?t={token}&c=phone-aaaaaaaa",
        base.replace("http://", "ws://")
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();
    let hello: serde_json::Value = serde_json::from_str(&next_text(&mut ws).await).unwrap();
    assert_eq!(hello["type"], "hello");
    // A client cannot present an id it was never told about.
    assert_eq!(hello["identify"]["param"], "c");

    let after = clients_until(&base, &token, |v| v["connections"] == 1).await;
    assert_eq!(after["browsers"]["state"], "reported");
    assert_eq!(after["browsers"]["count"], 1);
    assert_eq!(after["clients"][0]["id"], "phone-aaaaaaaa");
    assert_eq!(after["clients"][0]["connected"], true);

    // Everything the snapshot said before is still where it was.
    let (_, body) = get(&format!("{base}/healthz?t={token}")).await;
    let whole: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(whole["type"], "player");
    assert!(whole.get("positionS").is_some());
    assert_eq!(whole["endpoint"], before["endpoint"]);
}

/// A client that presents nothing is reported as unidentified, and the device
/// count degrades to a floor rather than guessing. This is today's default —
/// no client sends an id yet.
#[tokio::test]
async fn a_client_that_does_not_identify_makes_the_count_a_floor() {
    let (base, _cmd, token) = spawn_full_stack(1).await;
    let ws_url = format!("{}/ws?t={token}", base.replace("http://", "ws://"));
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();
    let _ = next_text(&mut ws).await;

    let view = clients_until(&base, &token, |v| v["connections"] == 1).await;
    assert_eq!(view["browsers"]["state"], "atLeast");
    assert_eq!(view["browsers"]["unidentified"], 1);
    assert_eq!(view["anyUnidentified"], true);
    assert_eq!(view["clients"][0]["identified"], false);
}

/// A `hello` sent after the socket opened claims it, and leaves no second row.
#[tokio::test]
async fn a_hello_over_the_socket_identifies_the_client() {
    let (base, _cmd, token) = spawn_full_stack(1).await;
    let ws_url = format!("{}/ws?t={token}", base.replace("http://", "ws://"));
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();
    let _ = next_text(&mut ws).await;

    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        r#"{"type":"hello","clientId":"phone-bbbbbbbb","label":"Test phone"}"#.into(),
    ))
    .await
    .unwrap();

    let view = clients_until(&base, &token, |v| v["browsers"]["state"] == "reported").await;
    assert_eq!(view["browsers"]["count"], 1);
    assert_eq!(view["clients"].as_array().unwrap().len(), 1, "no ghost row");
    assert_eq!(view["clients"][0]["label"], "Test phone");
}

/// A dropped socket stops counting as connected. It leaves a row saying it was
/// here, which is what makes a reconnect readable rather than inferred.
#[tokio::test]
async fn a_disconnect_stops_counting_but_leaves_a_trace() {
    let (base, _cmd, token) = spawn_full_stack(1).await;
    let ws_url = format!(
        "{}/ws?t={token}&c=phone-cccccccc",
        base.replace("http://", "ws://")
    );
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();
    let _ = next_text(&mut ws).await;
    ws.close(None).await.unwrap();

    let view = clients_until(&base, &token, |v| v["connections"] == 0).await;
    assert_eq!(view["clients"][0]["connected"], false);
    assert!(view["clients"][0]["disconnectedAtMs"].is_number());
}
