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
use crate::state::{LinkState, PlayerCommand, PlayerPacket, PlayerSnapshot};
use crate::{log_debug, log_info, log_warn};

/// How long to wait for the TCP handshake before giving up and retrying.
/// MFP uses 500 ms, but it is scanning localhost; a headset across Wi-Fi
/// deserves more slack.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

const RECONNECT_DELAY_MIN: Duration = Duration::from_secs(1);
const RECONNECT_DELAY_MAX: Duration = Duration::from_secs(15);

/// Dial `endpoint` forever, publishing state into `snapshot_tx`.
///
/// Never returns. The player being absent is the expected steady state during
/// development, so a failed connection is not an error — it is logged once at
/// WARN and thereafter at DEBUG until something changes.
pub async fn run(
    endpoint: String,
    snapshot_tx: watch::Sender<PlayerSnapshot>,
    mut cmd_rx: mpsc::Receiver<PlayerCommand>,
) {
    let mut delay = RECONNECT_DELAY_MIN;
    // Only the transition into a failing state is worth a WARN; after that the
    // log would be nothing but "still not there" once a second.
    let mut announced_failure = false;

    loop {
        snapshot_tx.send_modify(|s| s.link = LinkState::Connecting);

        match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(&endpoint)).await {
            Ok(Ok(stream)) => {
                // The control channel is small and latency-sensitive; Nagle
                // would coalesce a seek command with the next heartbeat.
                if let Err(e) = stream.set_nodelay(true) {
                    log_debug!("[player] set_nodelay failed: {e}");
                }
                log_info!("[player] connected to {endpoint}");
                announced_failure = false;
                delay = RECONNECT_DELAY_MIN;

                snapshot_tx.send_modify(|s| {
                    s.link = LinkState::Connected;
                    s.packets = 0;
                });

                let reason = serve_connection(stream, &snapshot_tx, &mut cmd_rx).await;
                log_info!("[player] disconnected from {endpoint}: {reason}");
                snapshot_tx.send_modify(|s| s.on_disconnect(now_ms()));
            }
            Ok(Err(e)) => report_unreachable(&endpoint, &e.to_string(), &mut announced_failure),
            Err(_) => report_unreachable(
                &endpoint,
                &format!("no response within {CONNECT_TIMEOUT:?}"),
                &mut announced_failure,
            ),
        }

        snapshot_tx.send_modify(|s| s.link = LinkState::Disconnected);
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(RECONNECT_DELAY_MAX);
    }
}

fn report_unreachable(endpoint: &str, detail: &str, announced: &mut bool) {
    if *announced {
        log_debug!("[player] {endpoint} still unreachable: {detail}");
    } else {
        log_warn!(
            "[player] cannot reach {endpoint}: {detail}. \
             Is the player running, and is remote control enabled in its settings? \
             Nothing listens on 23554 until that box is ticked."
        );
        *announced = true;
    }
}

/// Drive one established connection until either half fails. Returns a human
/// description of why it ended.
async fn serve_connection(
    stream: TcpStream,
    snapshot_tx: &watch::Sender<PlayerSnapshot>,
    cmd_rx: &mut mpsc::Receiver<PlayerCommand>,
) -> String {
    let (reader, writer) = stream.into_split();

    let read_tx = snapshot_tx.clone();
    let mut read_task = tokio::spawn(async move { read_loop(reader, read_tx).await });

    let outcome = tokio::select! {
        joined = &mut read_task => match joined {
            Ok(reason) => reason,
            Err(e) => format!("read task panicked: {e}"),
        },
        reason = write_loop(writer, cmd_rx) => reason,
    };

    read_task.abort();
    outcome
}

/// Read frames until the stream ends or desyncs.
async fn read_loop<R>(mut reader: R, snapshot_tx: watch::Sender<PlayerSnapshot>) -> String
where
    R: AsyncRead + Unpin,
{
    loop {
        match codec::read_frame(&mut reader).await {
            Ok(Frame::Heartbeat) => {
                log_debug!("[player] <- heartbeat");
            }
            Ok(Frame::Json(json)) => {
                // Log the raw payload. During a spike this is the whole point:
                // it is how we find out what a real Quest actually sends, as
                // opposed to what the docs say it sends.
                log_debug!("[player] <- {json}");

                match serde_json::from_str::<PlayerPacket>(&json) {
                    Ok(packet) => {
                        snapshot_tx.send_modify(|s| s.apply(&packet, now_ms()));
                    }
                    Err(e) => {
                        // Do not tear the connection down over one bad packet.
                        log_warn!("[player] unparseable packet ({e}): {json}");
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return "player closed the connection".into();
            }
            Err(e) => return format!("read error: {e}"),
        }
    }
}

/// Send a heartbeat every second, and forward any queued command.
///
/// The player closes the connection if it hears nothing for 3 s, so this loop
/// failing to run is indistinguishable from the bridge crashing, from the
/// player's point of view.
async fn write_loop<W>(mut writer: W, cmd_rx: &mut mpsc::Receiver<PlayerCommand>) -> String
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

        let task = tokio::spawn(read_loop(bridge, tx));

        codec::write_json(&mut peer, "not json at all")
            .await
            .unwrap();
        codec::write_json(&mut peer, r#"{"currentTime":7.5}"#)
            .await
            .unwrap();
        drop(peer);

        let reason = task.await.unwrap();
        assert!(reason.contains("closed"), "unexpected reason: {reason}");
        assert_eq!(rx.borrow().position_s, Some(7.5));
    }

    /// A heartbeat between two JSON packets must not shift the framing.
    #[tokio::test]
    async fn read_loop_handles_interleaved_heartbeats() {
        let (mut peer, bridge) = duplex(4096);
        let (tx, rx) = watch::channel(PlayerSnapshot::new("test".into()));
        let task = tokio::spawn(read_loop(bridge, tx));

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
        tokio::spawn(async move { write_loop(bridge, &mut cmd_rx).await });

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
        tokio::spawn(async move { write_loop(bridge, &mut cmd_rx).await });

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
