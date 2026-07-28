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

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::{TcpListener, TcpStream};

use crate::codec::{self, Frame};
use crate::logging::now_ms;
use crate::state::PlayerPacket;
use crate::{log_info, log_warn};

/// The real players' documented tolerance before they hang up.
pub const CLIENT_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone)]
pub struct FakePlayerConfig {
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
            media_path: "C:\\VR\\fake-clip.mp4".into(),
            duration_s: 600.0,
            tick: Duration::from_millis(500),
            full_packet_every: 8,
            send_heartbeats: true,
            enforce_timeout: true,
        }
    }
}

/// Accept connections forever, serving one at a time (as the real players do).
pub async fn serve(listener: TcpListener, cfg: FakePlayerConfig) {
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                log_info!("[fake-player] client connected from {peer}");
                let reason = serve_client(stream, &cfg).await;
                log_info!("[fake-player] client {peer} gone: {reason}");
            }
            Err(e) => {
                log_warn!("[fake-player] accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    }
}

/// Serve one client until it disconnects or times out. Returns why it ended.
pub async fn serve_client(stream: TcpStream, cfg: &FakePlayerConfig) -> String {
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
        tokio::spawn(async move {
            loop {
                match codec::read_frame(&mut reader).await {
                    Ok(frame) => {
                        last_rx.store(now_ms(), Ordering::Relaxed);
                        if let Frame::Json(json) = frame {
                            log_info!("[fake-player] <- {json}");
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
                    Err(_) => return,
                }
            }
        })
    };

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
                if cfg.send_heartbeats && ticks.is_multiple_of(4) {
                    if let Err(e) = codec::write_heartbeat(&mut writer).await {
                        read_task.abort();
                        return format!("heartbeat write failed: {e}");
                    }
                }
            }
        }
    }
}
