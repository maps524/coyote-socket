//! The DeoVR / HereSphere client: dial the player, keep the link alive, and
//! publish what it says.
//!
//! One adapter covers both players because their framing and field names are
//! identical (see `codec.rs` for the provenance of that claim).
//!
//! Shape of a connection:
//!
//! ```text
//!   connect (3 s timeout)
//!     ├── read task   : read_frame → PlayerPacket → merge into snapshot
//!     └── write task  : 1 Hz heartbeat, plus any queued PlayerCommand
//!   either side ending tears down both, then we back off and redial.
//! ```
//!
//! The read and write halves are separate tasks rather than two arms of one
//! `select!` because `read_frame` is not cancel-safe mid-frame: a keepalive
//! tick firing while a length prefix had been consumed but the payload had
//! not would desync the stream permanently.

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};

use crate::codec::{self, Frame, KEEPALIVE_INTERVAL_MS};
use crate::logging::now_ms;
use crate::probe;
use crate::state::{
    classify_connect_error, FaultKind, LinkFault, LinkState, PlayerCommand, PlayerPacket,
    PlayerSnapshot,
};
use crate::wire::{Dir, Kind, Source, Tap};
use crate::{log_debug, log_info, log_warn};

/// How long to wait for the TCP handshake before giving up and retrying.
/// MFP uses 500 ms, but it is scanning localhost; a headset across Wi-Fi
/// deserves more slack.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

const RECONNECT_DELAY_MIN: Duration = Duration::from_secs(1);
const RECONNECT_DELAY_MAX: Duration = Duration::from_secs(15);

/// Dial `endpoint` until told otherwise, publishing state into `snapshot_tx`.
///
/// Never returns on its own — the caller cancels it to disconnect. Dropping
/// this future closes the socket, which is what a "Disconnect" button wants;
/// the read half is a separate task precisely so that cancellation happens at
/// the connection level and never mid-frame.
///
/// `cmd_rx` is borrowed rather than owned so the supervisor can hand the same
/// queue to the next connection instead of rebuilding it, which would drop
/// anything queued during a reconnect.
pub async fn run(
    endpoint: String,
    snapshot_tx: &watch::Sender<PlayerSnapshot>,
    cmd_rx: &mut mpsc::Receiver<PlayerCommand>,
    tap: Tap,
) {
    let mut delay = RECONNECT_DELAY_MIN;
    // Only the transition into a failing state is worth a WARN; after that the
    // log would be nothing but "still not there" once a second.
    let mut announced_failure = false;

    loop {
        snapshot_tx.send_modify(|s| {
            s.link = LinkState::Connecting;
            s.attempts = s.attempts.saturating_add(1);
        });

        match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(&endpoint)).await {
            Ok(Ok(stream)) => {
                // The control channel is small and latency-sensitive; Nagle
                // would coalesce a seek command with the next heartbeat.
                if let Err(e) = stream.set_nodelay(true) {
                    log_debug!("[player] set_nodelay failed: {e}");
                }
                log_info!("[player] connected to {endpoint}");
                tap.event(Source::Player, format!("connected to {endpoint}"));
                announced_failure = false;
                delay = RECONNECT_DELAY_MIN;

                snapshot_tx.send_modify(|s| {
                    s.link = LinkState::Connected;
                    s.packets = 0;
                    s.fault = None;
                    // The discontinuity marker, bumped before any packet from
                    // this connection can be merged in. A consumer that sees a
                    // new epoch must treat the position that follows as
                    // unrelated to the one before it.
                    s.epoch = s.epoch.wrapping_add(1);
                });

                let outcome = serve_connection(stream, snapshot_tx, cmd_rx, &tap).await;
                log_info!("[player] disconnected from {endpoint}: {}", outcome.detail);
                tap.event(
                    Source::Player,
                    format!("disconnected: {}", outcome.detail),
                );
                let fault = LinkFault::new(outcome.kind, outcome.detail);
                snapshot_tx.send_modify(|s| {
                    s.on_disconnect(now_ms());
                    s.fault = Some(fault);
                });
            }
            Ok(Err(e)) => {
                let fault = refine(&endpoint, classify_connect_error(&e)).await;
                report_unreachable(&endpoint, &fault, &mut announced_failure, &tap);
                snapshot_tx.send_modify(|s| s.fault = Some(fault));
            }
            Err(_) => {
                let fault = refine(
                    &endpoint,
                    LinkFault::new(
                        FaultKind::TimedOut,
                        format!("no response within {CONNECT_TIMEOUT:?}"),
                    ),
                )
                .await;
                report_unreachable(&endpoint, &fault, &mut announced_failure, &tap);
                snapshot_tx.send_modify(|s| s.fault = Some(fault));
            }
        }

        snapshot_tx.send_modify(|s| s.link = LinkState::Retrying);
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(RECONNECT_DELAY_MAX);
    }
}

/// Turn a bare "no answer" into something that says whether the host is there.
///
/// Only run on failure. Probing before every attempt would mean opening a
/// second connection to a player that accepts one at a time, which risks
/// breaking the very link we are trying to establish.
async fn refine(endpoint: &str, fault: LinkFault) -> LinkFault {
    if fault.kind != FaultKind::TimedOut {
        return fault;
    }
    let reachability = probe::probe(endpoint).await;
    match reachability.as_fault() {
        // Keep the original error text — it is the evidence — but take the
        // sharper explanation the probe found.
        Some(refined) => LinkFault {
            kind: refined.kind,
            detail: format!("{} ({})", fault.detail, reachability.summary()),
            hint: refined.hint,
        },
        None => fault,
    }
}

fn report_unreachable(endpoint: &str, fault: &LinkFault, announced: &mut bool, tap: &Tap) {
    if *announced {
        log_debug!("[player] {endpoint} still unreachable: {}", fault.detail);
        return;
    }
    let hint = fault.hint.as_deref().unwrap_or("");
    log_warn!("[player] cannot reach {endpoint}: {}. {hint}", fault.detail);
    tap.event(
        Source::Player,
        format!("cannot reach {endpoint}: {}", fault.detail),
    );
    *announced = true;
}

/// Whether an I/O error means "the peer went away" rather than "the bytes were
/// wrong".
///
/// A headset that sleeps, a Wi-Fi drop and a player that quits all land here,
/// and none of them says anything about the protocol.
fn is_disconnect(e: &std::io::Error) -> bool {
    use std::io::ErrorKind::*;
    matches!(
        e.kind(),
        UnexpectedEof | ConnectionReset | ConnectionAborted | BrokenPipe | TimedOut | NotConnected
    )
}

/// Why a connection ended, and whether that reason implicates the protocol.
struct Outcome {
    kind: FaultKind,
    detail: String,
}

impl Outcome {
    fn closed(detail: impl Into<String>) -> Self {
        Self {
            kind: FaultKind::Closed,
            detail: detail.into(),
        }
    }
    fn framing(detail: impl Into<String>) -> Self {
        Self {
            kind: FaultKind::Framing,
            detail: detail.into(),
        }
    }
}

/// A `JoinHandle` that aborts its task when dropped.
///
/// Tokio's `JoinHandle` **detaches** on drop rather than aborting, which is a
/// reasonable default and exactly the wrong one here. The read task owns the
/// `OwnedReadHalf` and a clone of `snapshot_tx`, so a detached one keeps
/// reading the socket and keeps publishing state — for a connection the user
/// has disconnected from.
///
/// The consequence was observed in the wild, not theorised: in the one real
/// session anyone has captured, a position frame was published **3.9 seconds
/// after the user pressed Disconnect** (`capture.rs`). `on_disconnect` had
/// already cleared position and set the link to `Idle`, and the ghost
/// immediately republished a live-advancing position underneath it — a
/// consumer reading `link: "idle"` alongside a moving position, which is
/// precisely the lying state `on_disconnect` exists to prevent.
///
/// It self-heals within ~3 s against DeoVR only because DeoVR enforces the
/// keepalive timeout and closes the socket. **HereSphere is unobserved.** A
/// peer that does not enforce it leaks the task forever, and switching
/// endpoints leaks another every time.
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl<T> std::future::Future for AbortOnDrop<T> {
    type Output = Result<T, tokio::task::JoinError>;
    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        std::pin::Pin::new(&mut self.0).poll(cx)
    }
}

/// Drive one established connection until either half fails.
///
/// Cancelling this future — which is how Disconnect and endpoint switching
/// work — must take the read task with it. See [`AbortOnDrop`].
async fn serve_connection(
    stream: TcpStream,
    snapshot_tx: &watch::Sender<PlayerSnapshot>,
    cmd_rx: &mut mpsc::Receiver<PlayerCommand>,
    tap: &Tap,
) -> Outcome {
    let (reader, writer) = stream.into_split();

    let read_tx = snapshot_tx.clone();
    let read_tap = tap.clone();
    let mut read_task = AbortOnDrop(tokio::spawn(read_loop(reader, read_tx, read_tap)));

    tokio::select! {
        joined = &mut read_task => match joined {
            Ok(outcome) => outcome,
            Err(e) if e.is_cancelled() => Outcome::closed("read task cancelled"),
            Err(e) => Outcome::closed(format!("read task panicked: {e}")),
        },
        detail = write_loop(writer, cmd_rx, tap) => Outcome::closed(detail),
    }
    // `read_task` is dropped here on every path, including the one where the
    // write half returned first. No explicit abort call to forget.
}

/// Read frames until the stream ends or desyncs.
async fn read_loop<R>(
    mut reader: R,
    snapshot_tx: watch::Sender<PlayerSnapshot>,
    tap: Tap,
) -> Outcome
where
    R: AsyncRead + Unpin,
{
    loop {
        match codec::read_frame_raw(&mut reader).await {
            Ok(raw) => match raw.frame {
                Frame::Heartbeat => {
                    log_debug!("[player] <- heartbeat");
                    tap.frame(
                        Source::Player,
                        Dir::In,
                        Kind::Heartbeat,
                        raw.prefix,
                        raw.len as i64,
                        None,
                        None,
                    );
                }
                Frame::Json(json) => {
                    // Log the raw payload. During a spike this is the whole
                    // point: it is how we find out what a real Quest actually
                    // sends, as opposed to what the docs say it sends.
                    log_debug!("[player] <- {json}");
                    let note = codec::diagnose_payload(&json);
                    if let Some(note) = &note {
                        log_warn!("[player] suspicious payload: {note}");
                    }
                    tap.frame(
                        Source::Player,
                        Dir::In,
                        if note.is_some() { Kind::Error } else { Kind::Json },
                        raw.prefix,
                        raw.len as i64,
                        Some(json.clone()),
                        note,
                    );

                    match serde_json::from_str::<PlayerPacket>(&json) {
                        Ok(packet) => {
                            snapshot_tx.send_modify(|s| s.apply(&packet, now_ms()));
                        }
                        Err(e) => {
                            // Do not tear the connection down over one bad
                            // packet: a field we do not model is far more
                            // likely than a broken stream, and dropping the
                            // link would hide the payload we came here to see.
                            log_warn!("[player] unparseable packet ({e}): {json}");
                        }
                    }
                }
            },
            Err(e) if is_disconnect(&e) => {
                return Outcome::closed(format!("player closed the connection: {e}"));
            }
            Err(e) => {
                // Only a genuine decode failure counts as framing. Getting
                // this wrong is expensive in both directions, and the first
                // real session proved it: a headset going to sleep produced
                // WSAECONNRESET, which was reported as "the stream did not fit
                // our reading of the framing" — the loudest alarm this app
                // has, raised for an ordinary disconnect. A framing alarm that
                // cries wolf is worse than no alarm, because the one time it
                // is right nobody will believe it.
                let detail = e.to_string();
                log_warn!("[player] framing failure: {detail}");
                tap.frame(
                    Source::Player,
                    Dir::In,
                    Kind::Error,
                    [0; 4],
                    0,
                    Some(detail.clone()),
                    Some(
                        "The stream did not fit our reading of the framing. This is the \
                         result the spike was looking for — copy the wire log."
                            .into(),
                    ),
                );
                return Outcome::framing(detail);
            }
        }
    }
}

/// Send a heartbeat every second, and forward any queued command.
///
/// The player closes the connection if it hears nothing for 3 s, so this loop
/// failing to run is indistinguishable from the bridge crashing, from the
/// player's point of view.
async fn write_loop<W>(
    mut writer: W,
    cmd_rx: &mut mpsc::Receiver<PlayerCommand>,
    tap: &Tap,
) -> String
where
    W: AsyncWrite + Unpin,
{
    let mut ticker = tokio::time::interval(Duration::from_millis(KEEPALIVE_INTERVAL_MS));
    // If we fall behind (debugger pause, machine sleep) we want one prompt
    // heartbeat, not a burst of catch-up ticks.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if let Err(e) = codec::write_heartbeat(&mut writer).await {
                    return format!("heartbeat write failed: {e}");
                }
            }
            // `Receiver::recv` is cancel-safe, so losing this arm to a tick
            // does not drop a queued command.
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else {
                    return "command channel closed".into();
                };
                let json = match serde_json::to_string(&cmd) {
                    Ok(j) => j,
                    Err(e) => {
                        log_warn!("[player] could not serialise command: {e}");
                        continue;
                    }
                };
                log_debug!("[player] -> {json}");
                tap.frame(
                    Source::Player,
                    Dir::Out,
                    Kind::Json,
                    (json.len() as i32).to_le_bytes(),
                    json.len() as i64,
                    Some(json.clone()),
                    None,
                );
                if let Err(e) = codec::write_json(&mut writer, &json).await {
                    return format!("command write failed: {e}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    /// The read loop must survive a packet it cannot parse. A player that
    /// sends one field we do not model should not knock the bridge offline.
    #[tokio::test]
    async fn read_loop_survives_a_malformed_packet() {
        let (mut peer, bridge) = duplex(4096);
        let (tx, rx) = watch::channel(PlayerSnapshot::new("test".into()));

        let task = tokio::spawn(read_loop(bridge, tx, Tap::disabled()));

        codec::write_json(&mut peer, "not json at all")
            .await
            .unwrap();
        codec::write_json(&mut peer, r#"{"currentTime":7.5}"#)
            .await
            .unwrap();
        drop(peer);

        let outcome = task.await.unwrap();
        assert_eq!(outcome.kind, FaultKind::Closed);
        assert_eq!(rx.borrow().position_s, Some(7.5));
    }

    /// A reset connection is a disconnect, not a framing failure.
    ///
    /// Regression test for a real false alarm: the first session against a
    /// Quest ended in WSAECONNRESET when the headset slept, and the app
    /// reported it as evidence that our reading of the protocol was wrong.
    #[test]
    fn a_reset_connection_is_not_a_framing_failure() {
        use std::io::ErrorKind::*;
        for kind in [ConnectionReset, ConnectionAborted, BrokenPipe, UnexpectedEof] {
            assert!(
                is_disconnect(&std::io::Error::new(kind, "peer went away")),
                "{kind:?} must not raise the framing alarm"
            );
        }
        // A decode failure still must.
        assert!(!is_disconnect(&std::io::Error::new(
            InvalidData,
            "length prefix nonsense"
        )));
    }

    /// A framing failure must be reported as a framing failure, not folded in
    /// with an ordinary disconnect. The whole point of the Quest test is being
    /// able to tell those apart.
    #[tokio::test]
    async fn a_desynced_stream_is_reported_as_framing_not_as_a_close() {
        let (mut peer, bridge) = duplex(4096);
        let (tx, _rx) = watch::channel(PlayerSnapshot::new("test".into()));
        let (tap, mut events) = Tap::channel(16);
        let task = tokio::spawn(read_loop(bridge, tx, tap));

        // A big-endian length prefix: exactly the failure the diagnostic
        // exists to name.
        use tokio::io::AsyncWriteExt;
        peer.write_all(&300i32.to_be_bytes()).await.unwrap();
        peer.write_all(&[b'x'; 8]).await.unwrap();

        let outcome = task.await.unwrap();
        assert_eq!(outcome.kind, FaultKind::Framing);
        assert!(
            outcome.detail.contains("byte order"),
            "the diagnosis must reach the caller: {}",
            outcome.detail
        );

        let event = events.recv().await.unwrap();
        assert_eq!(event.kind, crate::wire::Kind::Error);
        assert!(event.note.is_some(), "the UI needs the explanation");
    }

    /// A payload that frames cleanly but is not JSON is the quiet version of
    /// the same failure, and must still be flagged.
    #[tokio::test]
    async fn a_non_json_payload_is_tapped_as_suspicious() {
        let (mut peer, bridge) = duplex(4096);
        let (tx, _rx) = watch::channel(PlayerSnapshot::new("test".into()));
        let (tap, mut events) = Tap::channel(16);
        tokio::spawn(read_loop(bridge, tx, tap));

        codec::write_json(&mut peer, "ath\":\"a.mp4\"}").await.unwrap();

        let event = events.recv().await.unwrap();
        assert_eq!(event.kind, crate::wire::Kind::Error);
        assert!(event.note.unwrap().contains("length prefix"));
        assert_eq!(event.prefix_hex, "0d 00 00 00");
    }

    /// A heartbeat between two JSON packets must not shift the framing.
    #[tokio::test]
    async fn read_loop_handles_interleaved_heartbeats() {
        let (mut peer, bridge) = duplex(4096);
        let (tx, rx) = watch::channel(PlayerSnapshot::new("test".into()));
        let task = tokio::spawn(read_loop(bridge, tx, Tap::disabled()));

        codec::write_json(&mut peer, r#"{"path":"a.mp4","playerState":0}"#)
            .await
            .unwrap();
        codec::write_heartbeat(&mut peer).await.unwrap();
        codec::write_json(&mut peer, r#"{"currentTime":3.0}"#)
            .await
            .unwrap();
        drop(peer);
        task.await.unwrap();

        let snap = rx.borrow();
        assert_eq!(snap.media.as_deref(), Some("a.mp4"));
        assert_eq!(snap.position_s, Some(3.0));
        assert_eq!(snap.playing, Some(true));
        assert_eq!(snap.packets, 2, "heartbeats are not packets");
    }

    /// The write loop must emit a heartbeat on the documented cadence. Uses a
    /// paused clock so the assertion is deterministic rather than timing out.
    #[tokio::test(start_paused = true)]
    async fn write_loop_heartbeats_at_1hz() {
        let (mut peer, bridge) = duplex(4096);
        let (_cmd_tx, mut cmd_rx) = mpsc::channel::<PlayerCommand>(4);
        tokio::spawn(async move { write_loop(bridge, &mut cmd_rx, &Tap::disabled()).await });

        // interval() fires immediately, then once per second.
        for _ in 0..3 {
            let frame = codec::read_frame(&mut peer).await.unwrap();
            assert_eq!(frame, Frame::Heartbeat);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn write_loop_forwards_commands_as_framed_json() {
        let (mut peer, bridge) = duplex(4096);
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<PlayerCommand>(4);
        tokio::spawn(async move { write_loop(bridge, &mut cmd_rx, &Tap::disabled()).await });

        // Drain the immediate first tick.
        assert_eq!(
            codec::read_frame(&mut peer).await.unwrap(),
            Frame::Heartbeat
        );

        cmd_tx
            .send(PlayerCommand {
                current_time: Some(42.0),
                ..Default::default()
            })
            .await
            .unwrap();

        match codec::read_frame(&mut peer).await.unwrap() {
            Frame::Json(j) => assert_eq!(j, r#"{"currentTime":42.0}"#),
            other => panic!("expected the command, got {other:?}"),
        }
    }
}
