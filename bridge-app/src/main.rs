//! Desktop shell for the bridge: a window, a tray, and a Connect button.
//!
//! Everything that speaks a protocol lives in the `coyote-bridge` library.
//! This crate is a front end and nothing else — it owns the window, the tray,
//! the settings file, and the plumbing that turns library channels into Tauri
//! events. Nothing here is required for the bridge to work, which is the
//! point: `cargo run -p coyote-bridge --no-default-features` still produces a
//! headless service for a home server with no windowing stack at all.
//!
//! ## What this app is for
//!
//! Making the headset test one action. Before it, the test was
//! `cargo run --bin coyote-bridge -- --player <ip>` followed by reading a log
//! file. After it: launch, pick the address, press Connect, and read the
//! answer off the window.
//!
//! ## What it deliberately does not do
//!
//! It does not make the protocol client more proven than the evidence allows.
//! The client has now spoken to a real DeoVR on a Quest — once, for fourteen
//! minutes, on one platform, exercising position, play, pause and seek. It has
//! never spoken to HereSphere at all, never seen a media change, and never
//! seen on-device media. A tidy window showing "Connected" would be an
//! excellent way to forget where that line sits, so the UI states the scope on
//! its face and the wire log stays one click away.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod settings;
mod tray;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use coyote_bridge::auth;
use coyote_bridge::supervisor::{self, BridgeHandle};
use coyote_bridge::wire::{Tap, WireEvent};
use coyote_bridge::{http, log_info, log_warn, logging};
use serde::Serialize;
use tauri::{Emitter, Manager};
use tokio::net::TcpListener;

use settings::Settings;

/// Events the frontend listens for. Named as constants because a typo in an
/// event name is silent on both sides.
pub const EVENT_PLAYER: &str = "player-state";
pub const EVENT_WIRE: &str = "wire-event";
pub const EVENT_LOG: &str = "log-line";

/// Backlog the wire tap keeps for a UI that is busy rendering. Large enough to
/// cover a burst of packets on connect without dropping the very frames the
/// test exists to capture.
const WIRE_BACKLOG: usize = 2048;

pub struct AppState {
    pub bridge: BridgeHandle,
    pub settings: Mutex<Settings>,
    pub settings_path: PathBuf,
    /// The built-in fake player, when running. Aborting the handle stops it.
    pub fake_player: Mutex<Option<FakePlayer>>,
    pub urls: Urls,
    /// Why the HTTP server is not up, when it is not. `None` means it is.
    pub http_error: Mutex<Option<String>>,
}

pub struct FakePlayer {
    /// Tauri's handle, not tokio's: the task is spawned on Tauri's runtime so
    /// it shares a reactor with everything else here.
    pub task: tauri::async_runtime::JoinHandle<()>,
    pub endpoint: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Urls {
    /// What the phone should open — the LAN address, not loopback.
    pub pairing: String,
    /// What this machine should open.
    pub local: String,
    pub http_port: u16,
}

impl AppState {
    pub fn save_settings(&self) {
        if let Ok(settings) = self.settings.lock() {
            settings.save(&self.settings_path);
        }
    }
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let config_dir = app
                .path()
                .app_config_dir()
                .unwrap_or_else(|_| std::env::temp_dir());
            // Same ring-buffer logger the headless binary uses, so a log
            // pasted from the window and one pulled off a server look
            // identical.
            logging::init(Some(config_dir.clone()));

            let settings_path = config_dir.join("bridge-settings.json");
            let mut settings = Settings::load(&settings_path);
            let http_port = settings.http_port;
            let static_dir = settings.static_dir.clone().map(PathBuf::from);
            // Mints on first run; persisted, so the phone's saved URL survives
            // a restart. See `settings::Settings::token`.
            let token = settings.token();
            settings.save(&settings_path);

            let advertised = http::local_ip().unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
            let base = format!("http://{advertised}:{http_port}");
            let urls = Urls {
                // Carries the token: this is the URL that grants access, which
                // is why it is treated as a secret in the UI rather than
                // displayed as a plain address.
                pairing: auth::with_token(&base, &token),
                local: format!("http://127.0.0.1:{http_port}"),
                http_port,
            };
            let allowed_hosts = vec![
                format!("{advertised}:{http_port}"),
                format!("127.0.0.1:{http_port}"),
                format!("localhost:{http_port}"),
            ];

            // One live tap, shared by the player client and the fake player,
            // so a cross-check run reads as a single conversation rather than
            // two disconnected logs.
            let (tap, wire_rx) = Tap::channel(WIRE_BACKLOG);

            // `block_on` rather than `spawn`: the handle is needed
            // synchronously to build the app state, and this returns as soon
            // as the supervisor task is spawned. Starting idle is deliberate —
            // an app that dials the last address before anyone pressed
            // anything is a surprise, and this one drives hardware.
            let bridge = tauri::async_runtime::block_on(async {
                supervisor::spawn(None, tap.clone())
            });

            let state = Arc::new(AppState {
                bridge: bridge.clone(),
                settings: Mutex::new(settings),
                settings_path,
                fake_player: Mutex::new(None),
                urls: urls.clone(),
                http_error: Mutex::new(None),
            });
            app.manage(Arc::clone(&state));

            // Durable capture, before anything else that can drop data. The
            // ring logger keeps a bounded window and the window keeps a
            // bounded list; this file keeps everything. A session against a
            // real player is the only evidence of its kind anyone has, and
            // losing it to a ring buffer would be an avoidable loss.
            capture_wire(&tap, config_dir.join("wire-capture.jsonl"));

            serve_http(
                app.handle().clone(),
                Arc::clone(&state),
                static_dir,
                http_port,
                token,
                allowed_hosts,
            );
            forward_events(app.handle().clone(), bridge, wire_rx);
            tray::install(app.handle(), &urls)?;

            // Redacted: this line ends up in the window's log pane, which has
            // a button that copies it for pasting into bug reports.
            log_info!(
                "[app] bridge window ready; phone should open {}",
                auth::redact_url(&urls.pairing)
            );
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::bridge_status,
            commands::connect,
            commands::disconnect,
            commands::check_reachability,
            commands::send_player_command,
            commands::pairing_qr,
            commands::log_history,
            commands::start_fake_player,
            commands::stop_fake_player,
            commands::open_external,
            commands::set_static_dir,
        ])
        .on_window_event(|window, event| {
            // Close hides rather than exits: the tray is the app's resting
            // state, and a bridge that dies when you tidy your desktop is not
            // much of a bridge. "Quit" in the tray menu is the way out.
            //
            // A window that vanishes is indistinguishable from a crash to
            // anyone who does not already know about the tray — and on
            // Windows 11 the icon it went to is hidden in the overflow by
            // default, so "look at the tray" is not obvious advice. Say where
            // it went, in the log and in the tray's own tooltip, rather than
            // relying on the user to guess.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
                log_info!(
                    "[app] window closed to the tray — the bridge is still running. \
                     Click the tray icon to bring it back, or use Quit in its menu to stop."
                );
                if let Some(tray) = window.app_handle().tray_by_id("bridge") {
                    let _ = tray.set_tooltip(Some(
                        "CoyoteSocket bridge — still running. Click to reopen the window.",
                    ));
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running the bridge app");
}

/// Append every wire event to a file that is never trimmed.
///
/// JSON Lines rather than the rendered text: the prefix bytes, the decoded
/// length and the payload stay separate fields, so a later diff against
/// `fake_player`'s output is a data operation rather than a parse of our own
/// log format. Append-only and flushed per line — the interesting session is
/// the one that ends in a crash.
fn capture_wire(tap: &Tap, path: PathBuf) {
    let Some(mut rx) = tap.subscribe() else { return };
    tauri::async_runtime::spawn(async move {
        use std::io::Write;
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let Ok(line) = serde_json::to_string(&event) else {
                        continue;
                    };
                    let file = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&path);
                    if let Ok(mut file) = file {
                        let _ = writeln!(file, "{line}");
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    log_warn!("[capture] lost {n} frames before they reached the capture file");
                }
                Err(_) => return,
            }
        }
    });
}

/// Serve the phone-facing HTTP + WebSocket surface, exactly as the headless
/// binary does. The QR is worthless if nothing answers on the other end.
fn serve_http(
    _app: tauri::AppHandle,
    state: Arc<AppState>,
    static_dir: Option<PathBuf>,
    port: u16,
    token: auth::Token,
    allowed_hosts: Vec<String>,
) {
    let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port);
    let pairing_url = state.urls.pairing.clone();
    let snapshot_rx = state.bridge.snapshot_rx.clone();
    let cmd_tx = state.bridge.cmd_tx.clone();

    // Persist a rotated token, so a rotation performed over TLS survives a
    // restart rather than silently reverting to the cleartext one it replaced.
    let rotate_state = Arc::clone(&state);
    let on_token_rotated: Box<dyn Fn(&auth::Token) + Send + Sync> =
        Box::new(move |fresh: &auth::Token| {
            if let Ok(mut settings) = rotate_state.settings.lock() {
                settings.token = Some(fresh.as_str().to_string());
            }
            rotate_state.save_settings();
        });

    tauri::async_runtime::spawn(async move {
        match TcpListener::bind(bind).await {
            Ok(listener) => {
                log_info!("[app] serving the phone app on http://{bind}");
                http::run(
                    listener,
                    Arc::new(http::Ctx {
                        snapshot_rx,
                        cmd_tx,
                        static_dir,
                        pairing_url,
                        token: std::sync::RwLock::new(token),
                        allowed_hosts,
                        on_token_rotated: Some(on_token_rotated),
                    }),
                )
                .await;
            }
            Err(e) => {
                // Not fatal: the player link is the thing under test, and it
                // works whether or not a phone can reach us. Record it so the
                // window can say the QR will not work rather than showing a
                // QR that leads nowhere.
                let message = format!("could not serve on {bind}: {e}");
                log_warn!("[app] {message}");
                if let Ok(mut slot) = state.http_error.lock() {
                    *slot = Some(message);
                }
            }
        }
    });
}

/// Pump the library's channels into Tauri events.
///
/// Three separate streams rather than one merged one: the frontend renders
/// them in three different places, and merging them here would only mean
/// splitting them again there.
fn forward_events(
    app: tauri::AppHandle,
    bridge: BridgeHandle,
    mut wire_rx: tokio::sync::broadcast::Receiver<WireEvent>,
) {
    let mut snapshots = bridge.snapshot_rx.clone();
    let snapshot_app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let snapshot = snapshots.borrow_and_update().clone();
            let _ = snapshot_app.emit(EVENT_PLAYER, snapshot);
            if snapshots.changed().await.is_err() {
                return;
            }
        }
    });

    let wire_app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            match wire_rx.recv().await {
                Ok(event) => {
                    let _ = wire_app.emit(EVENT_WIRE, event);
                }
                // Lagged: the UI could not keep up. Say so in the log rather
                // than leaving a silent hole in the evidence.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    log_warn!("[app] wire view fell behind and lost {n} frames");
                }
                Err(_) => return,
            }
        }
    });

    // The ring logger only writes to disk every tenth line, which is fine for
    // a service that logs steadily and useless for a window that is idle
    // between tests. Flush on a timer so `coyote-bridge.log` is worth opening
    // at any moment, not only after a burst.
    tauri::async_runtime::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(2));
        loop {
            ticker.tick().await;
            logging::flush_now();
        }
    });

    let mut log_rx = logging::subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            match log_rx.recv().await {
                Ok(line) => {
                    let _ = app.emit(EVENT_LOG, line);
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return,
            }
        }
    });
}
