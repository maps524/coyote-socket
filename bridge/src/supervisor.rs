//! Connect and disconnect on demand.
//!
//! The spike dialled one endpoint, taken from a command-line flag, forever.
//! That is the right shape for a headless service and the wrong shape for a UI
//! with a Connect button, so the retry loop is wrapped in a task that can be
//! told to stop, or to point somewhere else.
//!
//! ## Why cancellation is safe here and not one layer down
//!
//! [`crate::player::run`] is cancelled at an arbitrary await point when the
//! endpoint changes. That is safe with respect to *framing*: there is no
//! half-read frame left behind, because the read task is torn down whole
//! rather than resumed mid-frame. It is specifically *not* the same as putting
//! `read_frame` in a losing `select!` arm, which would consume a length prefix
//! and then abandon its payload, desyncing a stream that stays open. The read
//! and write halves remain separate tasks inside a connection for exactly that
//! reason.
//!
//! That safety is **not** automatic, and an earlier version of this comment
//! claimed more than the code delivered. Cancelling this future drops a
//! `JoinHandle`, and dropping a tokio `JoinHandle` *detaches* the task rather
//! than aborting it — so the read half survived cancellation and kept
//! publishing state for a connection the user had disconnected from. The read
//! task is now held in an abort-on-drop guard; see `player::AbortOnDrop` for
//! the evidence, which was observed in a real capture rather than reasoned
//! about.
//!
//! ## Why the command queue is owned here
//!
//! `cmd_rx` lives in the supervisor and is lent to each connection. A queued
//! seek therefore survives a reconnect instead of being dropped with the
//! channel.

use tokio::sync::{mpsc, watch};

use crate::player;
use crate::state::{LinkState, PlayerCommand, PlayerSnapshot};
use crate::wire::{Source, Tap};
use crate::{log_info, logging::now_ms};

/// What the UI can ask the link to do.
#[derive(Debug, Clone)]
pub enum LinkCommand {
    /// Connect to `endpoint`, replacing any current connection.
    Connect { endpoint: String },
    /// Stop, and stay stopped.
    Disconnect,
}

/// Handles for driving and observing the bridge.
///
/// Cloneable so a UI layer can hand pieces to different tasks without an
/// `Arc<Mutex<…>>` around the whole thing.
#[derive(Clone)]
pub struct BridgeHandle {
    pub snapshot_rx: watch::Receiver<PlayerSnapshot>,
    /// Commands aimed at the *player* — seek, play, pause.
    pub cmd_tx: mpsc::Sender<PlayerCommand>,
    /// Commands aimed at the *link* — connect, disconnect.
    pub link_tx: mpsc::Sender<LinkCommand>,
    pub tap: Tap,
}

impl BridgeHandle {
    pub async fn connect(&self, endpoint: impl Into<String>) -> Result<(), String> {
        self.link_tx
            .send(LinkCommand::Connect {
                endpoint: endpoint.into(),
            })
            .await
            .map_err(|_| "the bridge task is gone".to_string())
    }

    pub async fn disconnect(&self) -> Result<(), String> {
        self.link_tx
            .send(LinkCommand::Disconnect)
            .await
            .map_err(|_| "the bridge task is gone".to_string())
    }

    pub fn snapshot(&self) -> PlayerSnapshot {
        self.snapshot_rx.borrow().clone()
    }
}

/// Start the supervisor on the current runtime and return handles to it.
///
/// `initial` connects immediately; `None` starts idle, which is what a UI
/// wants — a bridge that dials the last-used address before anyone pressed
/// anything is a surprise.
pub fn spawn(initial: Option<String>, tap: Tap) -> BridgeHandle {
    let endpoint_label = initial.clone().unwrap_or_default();
    // watch: consumers want current state, not a backlog. A slow client that
    // misses intermediate positions is fine — it gets the latest.
    let (snapshot_tx, snapshot_rx) = watch::channel(PlayerSnapshot::new(endpoint_label));
    // mpsc: commands are discrete and must not be coalesced.
    let (cmd_tx, cmd_rx) = mpsc::channel::<PlayerCommand>(16);
    let (link_tx, link_rx) = mpsc::channel::<LinkCommand>(8);

    let task_tap = tap.clone();
    tokio::spawn(async move { run(initial, snapshot_tx, cmd_rx, link_rx, task_tap).await });

    BridgeHandle {
        snapshot_rx,
        cmd_tx,
        link_tx,
        tap,
    }
}

/// The supervisor loop. Returns when the command channel closes.
pub async fn run(
    initial: Option<String>,
    snapshot_tx: watch::Sender<PlayerSnapshot>,
    mut cmd_rx: mpsc::Receiver<PlayerCommand>,
    mut link_rx: mpsc::Receiver<LinkCommand>,
    tap: Tap,
) {
    let mut target = initial;

    loop {
        let Some(endpoint) = target.clone() else {
            // Idle: nothing is dialling, so the only thing to wait for is an
            // instruction.
            match link_rx.recv().await {
                Some(LinkCommand::Connect { endpoint }) => {
                    target = Some(begin(&snapshot_tx, &tap, endpoint));
                }
                Some(LinkCommand::Disconnect) => {}
                None => return,
            }
            continue;
        };

        let next = tokio::select! {
            // Never returns on its own; it is here to be cancelled.
            _ = player::run(endpoint.clone(), &snapshot_tx, &mut cmd_rx, tap.clone()) => None,
            command = link_rx.recv() => match command {
                Some(LinkCommand::Connect { endpoint: next }) if next == endpoint => {
                    // Re-connecting to the same place is a "try again now"
                    // rather than a no-op: it skips the backoff.
                    Some(begin(&snapshot_tx, &tap, next))
                }
                Some(LinkCommand::Connect { endpoint: next }) => Some(begin(&snapshot_tx, &tap, next)),
                Some(LinkCommand::Disconnect) => {
                    log_info!("[bridge] disconnecting from {endpoint}");
                    tap.event(Source::Player, format!("disconnected from {endpoint} by request"));
                    snapshot_tx.send_modify(|s| {
                        s.on_disconnect(now_ms());
                        s.link = LinkState::Idle;
                        s.fault = None;
                        s.attempts = 0;
                    });
                    None
                }
                None => return,
            },
        };

        target = next;
    }
}

/// Reset state for a fresh attempt at `endpoint` and announce it.
fn begin(snapshot_tx: &watch::Sender<PlayerSnapshot>, tap: &Tap, endpoint: String) -> String {
    log_info!("[bridge] connecting to {endpoint}");
    tap.event(Source::Player, format!("connecting to {endpoint}"));
    snapshot_tx.send_modify(|s| {
        s.on_disconnect(now_ms());
        s.endpoint = endpoint.clone();
        s.link = LinkState::Connecting;
        s.fault = None;
        s.attempts = 0;
    });
    endpoint
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_player::{serve, FakePlayerConfig};
    use std::time::Duration;
    use tokio::net::TcpListener;

    async fn spawn_fake() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve(listener, FakePlayerConfig::default(), Tap::disabled()));
        addr.to_string()
    }

    async fn wait_for(
        rx: &mut watch::Receiver<PlayerSnapshot>,
        what: &str,
        predicate: impl Fn(&PlayerSnapshot) -> bool,
    ) -> PlayerSnapshot {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if predicate(&rx.borrow_and_update()) {
                return rx.borrow().clone();
            }
            tokio::time::timeout_at(deadline, rx.changed())
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
                .expect("channel closed");
        }
    }

    #[tokio::test]
    async fn starts_idle_and_connects_only_when_asked() {
        let handle = spawn(None, Tap::disabled());
        assert_eq!(handle.snapshot().link, LinkState::Idle);

        let endpoint = spawn_fake().await;
        handle.connect(&endpoint).await.unwrap();

        let mut rx = handle.snapshot_rx.clone();
        let snap = wait_for(&mut rx, "the link to come up", |s| {
            s.link == LinkState::Connected && s.position_s.is_some()
        })
        .await;
        assert_eq!(snap.endpoint, endpoint);
        assert!(snap.fault.is_none(), "a live link must carry no fault");
    }

    #[tokio::test]
    async fn disconnect_stops_dialling_and_clears_playback() {
        let endpoint = spawn_fake().await;
        let handle = spawn(Some(endpoint), Tap::disabled());
        let mut rx = handle.snapshot_rx.clone();
        wait_for(&mut rx, "the link to come up", |s| {
            s.link == LinkState::Connected
        })
        .await;

        handle.disconnect().await.unwrap();
        let snap = wait_for(&mut rx, "the link to go idle", |s| s.link == LinkState::Idle).await;
        assert_eq!(snap.position_s, None, "stale position must not linger");
        assert!(snap.fault.is_none(), "a deliberate stop is not a fault");
    }

    /// Switching endpoints must actually move, not leave the old connection
    /// running underneath.
    #[tokio::test]
    async fn connecting_elsewhere_replaces_the_current_link() {
        let first = spawn_fake().await;
        let second = spawn_fake().await;

        let handle = spawn(Some(first.clone()), Tap::disabled());
        let mut rx = handle.snapshot_rx.clone();
        wait_for(&mut rx, "the first link", |s| {
            s.link == LinkState::Connected && s.endpoint == first
        })
        .await;

        handle.connect(&second).await.unwrap();
        let snap = wait_for(&mut rx, "the second link", |s| {
            s.link == LinkState::Connected && s.endpoint == second
        })
        .await;
        assert_eq!(snap.endpoint, second);
    }

    /// **The ghost regression.** After Disconnect, nothing may publish state.
    ///
    /// Uses a peer that keeps sending and never enforces the keepalive
    /// timeout. That distinction is the whole point: against DeoVR the leaked
    /// read task self-healed within ~3 s because DeoVR closes the socket, which
    /// is why the existing `fake_player` tests could not see this. HereSphere
    /// is unobserved, and a peer that does not close leaks forever.
    ///
    /// The observed symptom was a position frame published 3.9 s after the
    /// user pressed Disconnect, republishing a live position under a link
    /// reporting `Idle`.
    #[tokio::test]
    async fn nothing_publishes_after_a_disconnect() {
        use tokio::io::AsyncWriteExt;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();

        // A player that talks forever and never hangs up.
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (_read, mut write) = stream.into_split();
            let mut position = 0.0f64;
            loop {
                position += 0.05;
                let json = format!(r#"{{"path":"ghost.mp4","currentTime":{position:.3}}}"#);
                if crate::codec::write_json(&mut write, &json).await.is_err() {
                    return;
                }
                let _ = write.flush().await;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });

        let handle = spawn(Some(addr), Tap::disabled());
        let mut rx = handle.snapshot_rx.clone();
        wait_for(&mut rx, "the link to come up", |s| {
            s.link == LinkState::Connected && s.position_s.is_some()
        })
        .await;

        handle.disconnect().await.unwrap();
        wait_for(&mut rx, "the link to go idle", |s| s.link == LinkState::Idle).await;

        // Give a leaked reader ample time to prove it is still there. The real
        // one published 3.9 s late; the peer above writes every 50 ms.
        let after_disconnect = handle.snapshot();
        tokio::time::sleep(Duration::from_millis(600)).await;
        let later = handle.snapshot();

        assert_eq!(
            later.position_s, None,
            "a detached read task republished position after Disconnect"
        );
        assert_eq!(later.link, LinkState::Idle, "the link must stay idle");
        assert_eq!(
            later.updated_at_ms, after_disconnect.updated_at_ms,
            "nothing may touch the snapshot after Disconnect"
        );
    }

    /// The same leak by the other route: switching endpoints must not leave the
    /// previous connection's reader writing into the shared channel. Each
    /// switch leaked a task, and every one of them published.
    #[tokio::test]
    async fn switching_endpoints_does_not_leave_the_old_reader_publishing() {
        use tokio::io::AsyncWriteExt;

        // A chatty peer that never closes, as above.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let noisy = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (_read, mut write) = stream.into_split();
            loop {
                if crate::codec::write_json(&mut write, r#"{"path":"OLD.mp4"}"#)
                    .await
                    .is_err()
                {
                    return;
                }
                let _ = write.flush().await;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });

        let handle = spawn(Some(noisy), Tap::disabled());
        let mut rx = handle.snapshot_rx.clone();
        wait_for(&mut rx, "the first link", |s| {
            s.media.as_deref() == Some("OLD.mp4")
        })
        .await;

        let quiet = spawn_fake().await;
        handle.connect(&quiet).await.unwrap();
        wait_for(&mut rx, "the second link", |s| {
            s.link == LinkState::Connected && s.endpoint == quiet
        })
        .await;

        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_ne!(
            handle.snapshot().media.as_deref(),
            Some("OLD.mp4"),
            "the previous connection's reader is still publishing"
        );
    }

    /// A refused connection must surface as an explained fault rather than as
    /// a silent retry, because "the player's remote control is off" is the
    /// single most common reason this fails.
    #[tokio::test]
    async fn a_dead_endpoint_produces_an_explained_fault() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dead = listener.local_addr().unwrap().to_string();
        drop(listener);

        let handle = spawn(Some(dead), Tap::disabled());
        let mut rx = handle.snapshot_rx.clone();
        let snap = wait_for(&mut rx, "a fault", |s| s.fault.is_some()).await;

        let fault = snap.fault.unwrap();
        assert_eq!(fault.kind, crate::state::FaultKind::Refused);
        assert!(fault.hint.unwrap().contains("remote control"));
        assert!(snap.attempts >= 1, "attempts must be visible to the UI");
    }
}
