//! Bridge entry point.
//!
//! The tokio runtime runs on a background thread and the tray owns the main
//! thread, because on Windows and macOS the tray's event loop must be on the
//! main thread and never returns. With `--no-tray` (or a build without the
//! `tray` feature) the main thread just parks instead.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use coyote_bridge::wire::Tap;
use coyote_bridge::{http, logging, supervisor};
use coyote_bridge::{log_info, log_warn};
use tokio::net::TcpListener;

const DEFAULT_PLAYER_PORT: u16 = 23554;
const DEFAULT_HTTP_PORT: u16 = 8787;

struct Args {
    player: String,
    bind: IpAddr,
    http_port: u16,
    static_dir: Option<PathBuf>,
    log_dir: Option<PathBuf>,
    tray: bool,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            // Loopback by default: useful when the player is on this machine,
            // and harmless when it is not. The Quest's IP has to be typed in,
            // exactly as MultiFunPlayer requires.
            player: format!("127.0.0.1:{DEFAULT_PLAYER_PORT}"),
            // The phone has to reach us, so listening only on loopback would
            // defeat the point. Same reasoning as the desktop app's T-Code
            // server binding 0.0.0.0.
            bind: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            http_port: DEFAULT_HTTP_PORT,
            static_dir: None,
            log_dir: None,
            tray: true,
        }
    }
}

const USAGE: &str = "\
coyote-bridge (spike) - DeoVR/HereSphere to phone bridge

USAGE:
  coyote-bridge [OPTIONS]

OPTIONS:
  --player <host[:port]>  Player's remote-control endpoint.
                          Port defaults to 23554. [default: 127.0.0.1:23554]
  --bind <ip>             Address to serve on. [default: 0.0.0.0]
  --http-port <port>      Port to serve on. [default: 8787]
  --static-dir <path>     Directory to serve - the PWA's `dist`.
  --log-dir <path>        Where to write coyote-bridge.log. [default: cwd]
  --no-tray               Do not create a tray icon (headless servers).
  -h, --help              Show this.

NOTE: DeoVR and HereSphere do not listen on 23554 until remote control is
      enabled in the player's own settings. If the bridge reports the player
      unreachable, check that first.
";

fn parse_args() -> Result<Args, String> {
    let mut args = Args::default();
    let mut it = std::env::args().skip(1);

    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{flag} expects a value"));
        match flag.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "--player" => {
                let raw = value()?;
                // Accept a bare host and fill in the well-known port; typing
                // the IP is already the fiddly part.
                args.player = if raw.contains(':') {
                    raw
                } else {
                    format!("{raw}:{DEFAULT_PLAYER_PORT}")
                };
            }
            "--bind" => {
                args.bind = value()?
                    .parse()
                    .map_err(|e| format!("--bind is not an IP address: {e}"))?
            }
            "--http-port" => {
                args.http_port = value()?
                    .parse()
                    .map_err(|e| format!("--http-port is not a port: {e}"))?
            }
            "--static-dir" => args.static_dir = Some(PathBuf::from(value()?)),
            "--log-dir" => args.log_dir = Some(PathBuf::from(value()?)),
            "--no-tray" => args.tray = false,
            other => return Err(format!("unknown argument: {other}\n\n{USAGE}")),
        }
    }
    Ok(args)
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };

    logging::init(args.log_dir.clone());

    if let Some(dir) = &args.static_dir {
        if !dir.is_dir() {
            log_warn!(
                "[main] --static-dir {} does not exist; serving the placeholder page instead",
                dir.display()
            );
        }
    }

    let advertised_ip = http::local_ip().unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
    let pairing_url = format!("http://{advertised_ip}:{}", args.http_port);
    let local_url = format!("http://127.0.0.1:{}", args.http_port);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");

    let bind_addr = SocketAddr::new(args.bind, args.http_port);
    let player_endpoint = args.player.clone();
    let static_dir = args.static_dir.clone();
    let pairing_for_http = pairing_url.clone();

    runtime.spawn(async move {
        // The headless binary connects on start and never stops trying: it is
        // a service, and there is nobody here to press a button. The
        // supervisor's Connect/Disconnect commands exist for the UI, and are
        // simply left unused.
        let bridge = supervisor::spawn(Some(player_endpoint), Tap::disabled());

        match TcpListener::bind(bind_addr).await {
            Ok(listener) => {
                log_info!("[main] serving on http://{bind_addr}");
                http::run(
                    listener,
                    Arc::new(http::Ctx {
                        snapshot_rx: bridge.snapshot_rx.clone(),
                        cmd_tx: bridge.cmd_tx.clone(),
                        static_dir,
                        pairing_url: pairing_for_http,
                    }),
                )
                .await;
            }
            Err(e) => {
                log_warn!("[main] could not bind {bind_addr}: {e}");
                std::process::exit(1);
            }
        }
    });

    log_info!("[main] player endpoint: {}", args.player);
    log_info!("[main] phone should open: {pairing_url}");
    log_info!("[main] pairing QR: {local_url}/pair");
    logging::flush_now();

    #[cfg(feature = "tray")]
    if args.tray {
        // Never returns.
        coyote_bridge::tray::run(pairing_url, local_url);
    }

    #[cfg(not(feature = "tray"))]
    {
        let _ = (args.tray, &pairing_url, &local_url);
    }

    // Headless: park the main thread. Ctrl-C ends the process.
    loop {
        std::thread::park();
    }
}
