//! A stand-in for DeoVR / HereSphere, so the 23554 client can be exercised
//! without a headset in the room.
//!
//! This is the honest answer to "is the protocol client tested?". It is tested
//! *against this*, and this was written from the same two sources as the
//! client (DeoVR's published docs and MFP's adapters). That makes the pair
//! self-consistent, not necessarily correct: a shared misreading would pass
//! every test here. Only a real player settles it.
//!
//! What it deliberately does replicate, because these are the behaviours most
//! likely to break a naive client:
//!
//! - **It enforces the 3 s timeout.** If the client stops sending keepalives
//!   the connection is dropped, exactly as the real player does. A client that
//!   forgets to heartbeat fails here rather than in the headset.
//! - **It sends partial packets.** Steady-state updates carry `currentTime`
//!   only; the full packet arrives on connect and occasionally after. A client
//!   that replaces state instead of merging loses the duration.
//! - **It interleaves zero-length heartbeat frames** with JSON ones.
//! - **It accepts commands** and acts on them, so the phone→player direction
//!   can be seen working.
//!
//! ## Breaking the circularity: point MultiFunPlayer at this
//!
//! The self-consistency problem above has one cheap answer that does not need
//! a headset. **MultiFunPlayer is an independent implementation**, written by
//! someone else from the same published protocol. Configure its DeoVR or
//! HereSphere media source to point at this fake instead of at a headset. If
//! MFP connects, reads position, and follows play/pause, then this fake's
//! framing has been validated by a third party — and the client, which is
//! tested against this fake, inherits that validation.
//!
//! Be precise about what that does and does not prove:
//!
//! - It **does** prove our *server* side frames packets the way a real
//!   third-party client expects to read them.
//! - It **does not** prove our *client* can read a real player. Only a real
//!   DeoVR or HereSphere settles that.
//!
//! It is still worth doing before the headset test, because if MFP already
//! agrees with us, a Quest failure isolates to the real player rather than to
//! our framing in general.
//!
//! Everything MFP sends is published on the [`crate::wire::Tap`] **before it
//! is parsed**, with the raw length bytes attached. Parsing is exactly what
//! would hide a framing disagreement, so the evidence has to be captured
//! upstream of it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::{TcpListener, TcpStream};

use crate::codec::{self, Frame};
use crate::logging::now_ms;
use crate::state::PlayerPacket;
use crate::wire::{Dir, Kind, Source, Tap};
use crate::{log_info, log_warn};

/// The real players' documented tolerance before they hang up.
pub const CLIENT_TIMEOUT: Duration = Duration::from_secs(3);

/// Replay the captured DeoVR session, looping, at the gaps it was recorded
/// with.
///
/// Payloads go out byte for byte as they arrived — not re-serialised from a
/// parsed struct. Round-tripping through our own types would quietly normalise
/// key order, number formatting and anything we failed to model, which is
/// exactly the material a recording is for.
async fn replay<W>(writer: &mut W, tap: &Tap) -> String
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let capture = crate::capture::Capture::deovr_quest();
    if capture.frames.is_empty() {
        return "capture is empty".into();
    }
    log_info!(
        "[fake-player] replaying {} captured frames from: {}",
        capture.frames.len(),
        capture.source
    );

    let gaps = capture.gaps_ms();
    loop {
        for (index, frame) in capture.frames.iter().enumerate() {
            if let Err(e) = codec::write_json(writer, &frame.json).await {
                return format!("write failed: {e}");
            }
            tap_out(tap, &frame.json);

            // The gap *after* this frame. Looping back to the first frame
            // reuses the last observed gap, which is the closest thing to
            // honest that a three-frame loop allows.
            let gap = gaps.get(index).copied().unwrap_or(1010);
            tokio::time::sleep(Duration::from_millis(gap)).await;
        }
    }
}

/// Publish a frame we are about to put on the wire, with the prefix we framed
/// it with. Shown next to what the client sends back, the two are readable as
/// a conversation.
fn tap_out(tap: &Tap, json: &str) {
    tap.frame(
        Source::Fake,
        Dir::Out,
        Kind::Json,
        (json.len() as i32).to_le_bytes(),
        json.len() as i64,
        Some(json.to_string()),
        None,
    );
}

/// What the fake pretends to be.
///
/// The distinction is not cosmetic. Until a real player was captured,
/// everything this fake did was a guess dressed as a test — and several of
/// those guesses were wrong (see `capture.rs`). Replay exists so the default
/// test material is *observed* rather than *imagined*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Profile {
    /// Generated traffic. Responds to seek and play/pause, so it is the one to
    /// use when driving the fake by hand — but every detail of its shape is
    /// our own invention.
    Synthetic,
    /// Replays the DeoVR-on-a-Quest capture verbatim, at the gaps it was
    /// actually recorded with, looping.
    ///
    /// **This is DeoVR's behaviour, and only DeoVR's.** HereSphere has never
    /// been connected to by anything in this repo. A test passing against this
    /// replay is evidence about one player, one version, one platform — not
    /// about the other player the same adapter claims to cover.
    ///
    /// Deliberately inert: it ignores commands, because a recording cannot
    /// respond and pretending otherwise would put invention back in. Use
    /// [`Profile::Synthetic`] for interactive work and this for regression.
    ReplayDeoVr,
}

#[derive(Debug, Clone)]
pub struct FakePlayerConfig {
    pub profile: Profile,
    pub media_path: String,
    pub duration_s: f64,
    /// How often to push a state update. DeoVR is roughly 1 Hz; faster makes
    /// manual testing less tedious.
    pub tick: Duration,
    /// Emit a full packet every N ticks; partial (`currentTime` only) between.
    pub full_packet_every: u32,
    /// Interleave zero-length frames.
    pub send_heartbeats: bool,
    /// Drop a client that goes quiet for [`CLIENT_TIMEOUT`].
    pub enforce_timeout: bool,
}

impl Default for FakePlayerConfig {
    fn default() -> Self {
        Self {
            profile: Profile::Synthetic,
            media_path: "C:\\VR\\fake-clip.mp4".into(),
            duration_s: 600.0,
            tick: Duration::from_millis(500),
            full_packet_every: 8,
            send_heartbeats: true,
            enforce_timeout: true,
        }
    }
}

impl FakePlayerConfig {
    /// Replay the real capture.
    pub fn replay_deovr() -> Self {
        Self {
            profile: Profile::ReplayDeoVr,
            ..Default::default()
        }
    }

    /// Generated traffic, but shaped the way the real player actually behaved:
    /// a full packet every second, no interleaved heartbeats, and a URL for a
    /// path.
    ///
    /// Between `Synthetic` (responsive but invented) and `ReplayDeoVr`
    /// (observed but inert), this is the one that is both responsive and
    /// shaped by evidence.
    pub fn observed_deovr() -> Self {
        let capture = crate::capture::Capture::deovr_quest();
        let first: crate::state::PlayerPacket = serde_json::from_str(&capture.frames[0].json)
            .expect("the embedded capture parses");
        Self {
            profile: Profile::Synthetic,
            media_path: first.path.unwrap_or_default(),
            duration_s: first.duration.unwrap_or(600.0),
            // ~1010 ms observed; 1000 is the documented cadence.
            tick: Duration::from_millis(1000),
            // Every observed frame was a full packet.
            full_packet_every: 1,
            // None observed from DeoVR.
            send_heartbeats: false,
            enforce_timeout: true,
        }
    }
}

/// Accept connections forever, serving one at a time (as the real players do).
pub async fn serve(listener: TcpListener, cfg: FakePlayerConfig, tap: Tap) {
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                log_info!("[fake-player] client connected from {peer}");
                tap.event(Source::Fake, format!("client connected from {peer}"));
                let reason = serve_client(stream, &cfg, &tap).await;
                log_info!("[fake-player] client {peer} gone: {reason}");
                tap.event(Source::Fake, format!("client {peer} gone: {reason}"));
            }
            Err(e) => {
                log_warn!("[fake-player] accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    }
}

/// Serve one client until it disconnects or times out. Returns why it ended.
pub async fn serve_client(stream: TcpStream, cfg: &FakePlayerConfig, tap: &Tap) -> String {
    let _ = stream.set_nodelay(true);
    let (reader, mut writer) = stream.into_split();

    let last_rx = Arc::new(AtomicU64::new(now_ms()));
    let seek = Arc::new(std::sync::Mutex::new(None::<f64>));
    let paused = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let mut read_task = {
        let last_rx = Arc::clone(&last_rx);
        let seek = Arc::clone(&seek);
        let paused = Arc::clone(&paused);
        let mut reader = reader;
        let tap = tap.clone();
        tokio::spawn(async move {
            loop {
                // Raw read, and tapped before any parsing. If a third-party
                // client disagrees with us about framing, the parse is exactly
                // what would swallow the evidence.
                match codec::read_frame_raw(&mut reader).await {
                    Ok(raw) => {
                        last_rx.store(now_ms(), Ordering::Relaxed);
                        match raw.frame {
                            Frame::Heartbeat => tap.frame(
                                Source::Fake,
                                Dir::In,
                                Kind::Heartbeat,
                                raw.prefix,
                                raw.len as i64,
                                None,
                                None,
                            ),
                            Frame::Json(json) => {
                                log_info!("[fake-player] <- {json}");
                                let note = codec::diagnose_payload(&json);
                                tap.frame(
                                    Source::Fake,
                                    Dir::In,
                                    if note.is_some() { Kind::Error } else { Kind::Json },
                                    raw.prefix,
                                    raw.len as i64,
                                    Some(json.clone()),
                                    note,
                                );
                                if let Ok(cmd) = serde_json::from_str::<PlayerPacket>(&json) {
                                    if let Some(t) = cmd.current_time {
                                        *seek.lock().unwrap() = Some(t);
                                    }
                                    if let Some(s) = cmd.player_state {
                                        paused.store(s == 1, Ordering::Relaxed);
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        // A client we could not frame is the finding, not a
                        // nuisance: say so instead of closing quietly.
                        if e.kind() != std::io::ErrorKind::UnexpectedEof {
                            log_warn!("[fake-player] framing failure from client: {e}");
                            tap.frame(
                                Source::Fake,
                                Dir::In,
                                Kind::Error,
                                [0; 4],
                                0,
                                Some(e.to_string()),
                                Some(
                                    "A third-party client framed a packet in a way we could \
                                     not read. That is evidence about our framing, not about \
                                     the client."
                                        .into(),
                                ),
                            );
                        }
                        return;
                    }
                }
            }
        })
    };

    if cfg.profile == Profile::ReplayDeoVr {
        let outcome = replay(&mut writer, tap).await;
        read_task.abort();
        return outcome;
    }

    let mut position = 0.0f64;
    let mut ticks: u32 = 0;

    // Full state on connect — the client has nothing yet.
    let opening = format!(
        r#"{{"path":"{}","duration":{},"currentTime":{},"playbackSpeed":1.0,"playerState":0}}"#,
        cfg.media_path.escape_default(),
        cfg.duration_s,
        position
    );
    if let Err(e) = codec::write_json(&mut writer, &opening).await {
        return format!("write failed: {e}");
    }
    tap_out(tap, &opening);

    let mut ticker = tokio::time::interval(cfg.tick);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await; // consume the immediate first tick

    loop {
        tokio::select! {
            _ = &mut read_task => return "client closed the connection".into(),
            _ = ticker.tick() => {
                if cfg.enforce_timeout {
                    let quiet_ms = now_ms().saturating_sub(last_rx.load(Ordering::Relaxed));
                    if quiet_ms > CLIENT_TIMEOUT.as_millis() as u64 {
                        read_task.abort();
                        return format!(
                            "no keepalive for {quiet_ms} ms — dropped, as a real player would"
                        );
                    }
                }

                if let Some(t) = seek.lock().unwrap().take() {
                    position = t.clamp(0.0, cfg.duration_s);
                    log_info!("[fake-player] seeked to {position}");
                }
                if !paused.load(Ordering::Relaxed) {
                    position = (position + cfg.tick.as_secs_f64()) % cfg.duration_s;
                }

                ticks += 1;
                let state = if paused.load(Ordering::Relaxed) { 1 } else { 0 };
                let json = if cfg.full_packet_every > 0 && ticks.is_multiple_of(cfg.full_packet_every) {
                    format!(
                        r#"{{"path":"{}","duration":{},"currentTime":{:.3},"playbackSpeed":1.0,"playerState":{}}}"#,
                        cfg.media_path.escape_default(), cfg.duration_s, position, state
                    )
                } else {
                    // The steady-state shape: position only.
                    format!(r#"{{"currentTime":{position:.3},"playerState":{state}}}"#)
                };

                if let Err(e) = codec::write_json(&mut writer, &json).await {
                    read_task.abort();
                    return format!("write failed: {e}");
                }
                tap_out(tap, &json);
                if cfg.send_heartbeats && ticks.is_multiple_of(4) {
                    if let Err(e) = codec::write_heartbeat(&mut writer).await {
                        read_task.abort();
                        return format!("heartbeat write failed: {e}");
                    }
                    tap.frame(
                        Source::Fake,
                        Dir::Out,
                        Kind::Heartbeat,
                        codec::HEARTBEAT,
                        0,
                        None,
                        None,
                    );
                }
            }
        }
    }
}
