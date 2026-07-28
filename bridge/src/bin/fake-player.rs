//! Stand-in for a Quest running DeoVR or HereSphere.
//!
//! Run this, point the bridge at it, and the whole path works with no headset:
//!
//! ```text
//!   cargo run --bin fake-player
//!   cargo run --bin coyote-bridge -- --player 127.0.0.1:23554
//! ```
//!
//! It enforces the real 3 s keepalive timeout, so a client that fails to
//! heartbeat gets dropped here rather than silently in the headset.

use std::net::SocketAddr;
use std::time::Duration;

use coyote_bridge::fake_player::{serve, FakePlayerConfig};
use coyote_bridge::{log_info, logging};
use tokio::net::TcpListener;

const USAGE: &str = "\
fake-player - a stand-in DeoVR/HereSphere remote-control server

USAGE:
  fake-player [OPTIONS]

OPTIONS:
  --bind <addr>       Address to listen on. [default: 127.0.0.1:23554]
  --media <path>      Media path to report. [default: C:\\VR\\fake-clip.mp4]
  --duration <secs>   Reported duration. [default: 600]
  --tick <ms>         State update interval. [default: 500]
  --no-timeout        Do not drop a client that stops sending keepalives.
                      (Use to prove the timeout is what disconnects a broken
                      client, by watching it survive without one.)
  --no-heartbeats     Do not send zero-length frames.
  -h, --help          Show this.
";

#[tokio::main]
async fn main() {
    logging::init(None);

    let mut bind: SocketAddr = "127.0.0.1:23554".parse().unwrap();
    let mut cfg = FakePlayerConfig::default();

    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || {
            it.next()
                .unwrap_or_else(|| fail(&format!("{flag} expects a value")))
        };
        match flag.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return;
            }
            "--bind" => {
                bind = value()
                    .parse()
                    .unwrap_or_else(|e| fail(&format!("--bind: {e}")))
            }
            "--media" => cfg.media_path = value(),
            "--duration" => {
                cfg.duration_s = value()
                    .parse()
                    .unwrap_or_else(|e| fail(&format!("--duration: {e}")))
            }
            "--tick" => {
                cfg.tick = Duration::from_millis(
                    value()
                        .parse()
                        .unwrap_or_else(|e| fail(&format!("--tick: {e}"))),
                )
            }
            "--no-timeout" => cfg.enforce_timeout = false,
            "--no-heartbeats" => cfg.send_heartbeats = false,
            other => fail(&format!("unknown argument: {other}\n\n{USAGE}")),
        }
    }

    let listener = match TcpListener::bind(bind).await {
        Ok(l) => l,
        Err(e) => fail(&format!("could not bind {bind}: {e}")),
    };

    log_info!("[fake-player] listening on {bind}");
    log_info!(
        "[fake-player] media={} duration={}s tick={:?} timeout_enforced={}",
        cfg.media_path,
        cfg.duration_s,
        cfg.tick,
        cfg.enforce_timeout
    );

    serve(listener, cfg).await;
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(2);
}
