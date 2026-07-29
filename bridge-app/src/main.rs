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
use coyote_bridge::{http, log_info, log_warn, logging, mdns, tls};
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
    /// Whether the phone-facing server is actually listening.
    ///
    /// Three states, not an `Option<String>`. It was the latter, where `None`
    /// meant both "fine" and "not asked yet" — so between the window
    /// appearing and the bind returning, the UI rendered a QR and reported no
    /// problem, for a port that might already be taken. That is not
    /// hypothetical here: running two bridges at once is exactly what happens
    /// while developing one, and 8787 is the first casualty.
    ///
    /// A field that promises a capability the runtime has not confirmed is a
    /// shape worth hunting for generally. `bridge-tls` hit the same one from
    /// the other direction — a trust-check page that rendered in full and then
    /// blamed the certificate for a listener that had never bound.
    pub http_status: Mutex<HttpStatus>,
    /// Whether the phone can get a secure context. See [`TlsStatus`].
    pub tls_status: Mutex<TlsStatus>,
    /// The serving context, once the listener is up. Holds the live token, so
    /// anything that needs the current pairing URL asks here rather than
    /// caching a copy that revocation cannot reach.
    pub http_ctx: Mutex<Option<Arc<http::Ctx>>>,
}

/// Whether the phone can actually reach us.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum HttpStatus {
    /// The bind has not returned yet. Distinct from success on purpose: a QR
    /// shown now is a promise nobody has checked.
    Starting,
    Serving,
    Failed { detail: String },
}

impl HttpStatus {
    pub fn is_serving(&self) -> bool {
        matches!(self, Self::Serving)
    }
}

/// Whether the phone can get a secure context, which is what Web Bluetooth
/// requires.
///
/// The same three states as [`HttpStatus`], and for the same reason. This began
/// as a `tls_ready: bool` on `Urls`, set from "a certificate was issued" before
/// the TLS listener had bound — so a taken 8443 left the window saying the
/// phone was ready to pair while nothing was listening. Exactly the shape
/// `HttpStatus` exists to prevent, reintroduced one field over.
///
/// `NotConfigured` is separate from `Failed` because they need different
/// sentences: one is a choice, the other is a fault.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum TlsStatus {
    /// The bind has not returned yet.
    Starting,
    /// Listening, with a certificate the phone can install.
    Serving,
    /// Deliberately off, or no certificate could be made.
    NotConfigured { detail: String },
    Failed { detail: String },
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
    /// What the phone should open — the LAN address, not loopback — **without
    /// the token**. The full URL is derived, because the token can be revoked
    /// at runtime and a captured string would go on advertising a dead one.
    pub pairing_base: String,
    /// What this machine should open.
    pub local: String,
    pub http_port: u16,
    pub https_port: u16,
}

impl AppState {
    /// The pairing URL with whatever token is current.
    ///
    /// Falls back to the settings copy before the listener is up, so the
    /// window has something to show during startup rather than a bare base
    /// URL that would not work if scanned.
    pub fn pairing_url(&self) -> String {
        if let Ok(guard) = self.http_ctx.lock() {
            if let Some(ctx) = guard.as_ref() {
                return auth::with_token(&self.urls.pairing_base, &ctx.token());
            }
        }
        let token = self
            .settings
            .lock()
            .ok()
            .and_then(|s| s.token.clone())
            .map(auth::Token::from_string);
        match token {
            Some(token) => auth::with_token(&self.urls.pairing_base, &token),
            None => self.urls.pairing_base.clone(),
        }
    }

    pub fn save_settings(&self) {
        if let Ok(mut settings) = self.settings.lock() {
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
            let library_dir = settings.library_dir.clone().map(PathBuf::from);
            // Mints on first run; persisted, so the phone's saved URL survives
            // a restart. See `settings::Settings::token`.
            let token = settings.token();
            settings.save(&settings_path);

            let https_port = settings.https_port;
            // `None` when there is no routable address, and it stays `None`.
            // Loopback was the old fallback and it is the worst option: it goes
            // into the certificate's IP SAN *and* gets announced as
            // `coyote.local`, so a phone that resolves the name connects to
            // itself and reports the bridge unreachable — while everything on
            // this machine looks correct.
            let advertised = http::local_ip();

            // The certificate has to exist before the QR is built, because it
            // decides where the QR points. A failure here costs the phone Web
            // Bluetooth and nothing else, so it is reported rather than fatal.
            // Kept separate from `prepared` so the window can distinguish
            // "deliberately off" from "could not make one", which need
            // different sentences.
            let mut tls_unavailable: Option<String> = None;
            let prepared = match tls::prepare(&config_dir, http_port, https_port, advertised) {
                Ok(prepared) => Some(prepared),
                Err(e) => {
                    log_warn!(
                        "[app] could not set up TLS ({e}); serving plain HTTP only. \
                         The phone will not be able to use Bluetooth until this is fixed."
                    );
                    tls_unavailable = Some(e);
                    None
                }
            };

            // Answers `coyote.local`, so the phone's saved URL survives DHCP
            // moving this machine. Windows' own responder is not usable here —
            // it advertises a virtual adapter. See `coyote_bridge::mdns`.
            // The responder follows the address. A certificate that moved
            // while the name did not would leave `coyote.local` stable and
            // wrong, which is worse than unstable and right — nothing about
            // that failure points at DNS.
            let (advertised_tx, advertised_rx) =
                tokio::sync::watch::channel(advertised.unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)));
            let advertise = tls::Advertise {
                pinned: None,
                mdns_tx: Some(advertised_tx),
            };
            let mut using_mdns = false;
            if prepared.is_some() {
                // Skipped without an address: announcing a name we cannot back
                // with a reachable one is worse than not answering.
                if let Some(responder) = advertised.and_then(|ip| mdns::start(ip, https_port)) {
                    using_mdns = true;
                    // Tauri's spawn, not tokio's: `setup()` runs before any
                    // runtime has been entered, so a bare `tokio::spawn` here
                    // panics with "there is no reactor running" — on the
                    // default launch path.
                    tauri::async_runtime::spawn(mdns::follow(responder, advertised_rx));
                }
            }

            // The name goes on the QR, not the address — now that the name is
            // proven on a real iPhone. It survives DHCP moving this machine,
            // which is the entire reason the responder exists; an IP would hand
            // the phone a bookmark that breaks on the next lease, and a changed
            // origin wipes OPFS, the PWA install and the Bluetooth grant.
            //
            // The address remains the fallback for networks that eat multicast.
            let pairing_host = if using_mdns {
                coyote_bridge::certs::BRIDGE_HOSTNAME.to_string()
            } else {
                advertised
                    .map(|ip| ip.to_string())
                    .unwrap_or_else(|| "127.0.0.1".to_string())
            };
            let base = format!("http://{pairing_host}:{http_port}");

            let urls = Urls {
                // Without the token. The full URL grants access, so it is
                // derived on demand from the live token rather than captured
                // here where revocation could not reach it.
                //
                // The path is part of the base: when TLS is up the QR points at
                // the install page on **plain HTTP**, not at HTTPS. A phone that
                // has not yet trusted the local CA meets a full-page certificate
                // interstitial with no route back to the instructions —
                // stranded exactly when it needs help. The install page hands it
                // on to HTTPS once trust is verified.
                pairing_base: coyote_bridge::install::pairing_base(&base, prepared.is_some()),
                local: format!("http://127.0.0.1:{http_port}"),
                http_port,
                https_port,
            };
            // Recorded before `tls_unavailable` is moved into `serve_http`.
            let tls_missing = tls_unavailable.is_some();

            // Every origin the phone can legitimately present. Omitting the
            // HTTPS ones would let the app load and then have its own
            // WebSocket refused, which reads as a bridge fault and is not one.
            let allowed_hosts = tls::browser_origins(advertised, http_port, https_port);

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
                http_status: Mutex::new(HttpStatus::Starting),
                // `Starting` even when no certificate exists: the listener has
                // not been attempted yet either way, and the state is corrected
                // below once the answer is actually known.
                tls_status: Mutex::new(TlsStatus::Starting),
                http_ctx: Mutex::new(None),
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
                library_dir,
                http_port,
                token,
                allowed_hosts,
                prepared,
                advertise,
                tls_unavailable,
            );
            forward_events(app.handle().clone(), bridge, wire_rx);
            tray::install(app.handle(), &urls)?;

            // Redacted: this line ends up in the window's log pane, which has
            // a button that copies it for pasting into bug reports.
            log_info!(
                "[app] bridge window ready; phone should open {}",
                auth::redact_url(&state.pairing_url())
            );
            if tls_missing {
                log_warn!(
                    "[app] no certificate — the phone can open the app but not use Bluetooth"
                );
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::bridge_status,
            commands::connect,
            commands::disconnect,
            commands::check_reachability,
            commands::send_player_command,
            commands::pairing_qr,
            commands::rotate_token,
            commands::log_history,
            commands::start_fake_player,
            commands::stop_fake_player,
            commands::open_external,
            commands::set_static_dir,
            commands::set_library_dir,
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
    library_dir: Option<PathBuf>,
    port: u16,
    token: auth::Token,
    allowed_hosts: Vec<String>,
    prepared: Option<tls::Prepared>,
    advertise: tls::Advertise,
    // Why no certificate exists, when none does. Carried so the window can say
    // which of the two it is — a choice or a fault.
    tls_unavailable: Option<String>,
) {
    let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port);
    let pairing_base = state.urls.pairing_base.clone();
    let https_port = state.urls.https_port;
    let snapshot_rx = state.bridge.snapshot_rx.clone();
    let cmd_tx = state.bridge.cmd_tx.clone();

    // Persist a rotated token, so a rotation performed over TLS survives a
    // restart rather than silently reverting to the cleartext one it replaced.
    let rotate_state = Arc::clone(&state);
    let on_token_rotated: Box<dyn Fn(&auth::Token) + Send + Sync> =
        Box::new(move |fresh: &auth::Token| {
            if let Ok(mut settings) = rotate_state.settings.lock() {
                // `set_token` rather than assigning the field: it also claims
                // the right to write this token, which is what stops another
                // instance's stale copy overwriting the revocation.
                settings.set_token(fresh);
            }
            rotate_state.save_settings();
        });

    tauri::async_runtime::spawn(async move {
        // Inside the runtime: the library's poller is a tokio task.
        let library = library_dir.map(coyote_bridge::library::Library::spawn);
        match TcpListener::bind(bind).await {
            Ok(listener) => {
                log_info!("[app] serving the phone app on http://{bind}");

                // Bind TLS *before* building the context, so `ctx.tls`
                // describes a listener that exists rather than one we meant to
                // start. With it the other way round, a port already in use
                // left the install page fully rendered and its trust check
                // reporting a stage-2 failure as "you missed the trust step" —
                // sending the user to reinstall a certificate that was never
                // the problem, with nothing anywhere saying HTTPS was not
                // running.
                let (https, tls_state) = match prepared {
                    Some(prepared) => {
                        match tls::bind(IpAddr::V4(Ipv4Addr::UNSPECIFIED), https_port).await {
                            Ok(listener) => (Some((listener, prepared)), TlsStatus::Serving),
                            Err(e) => {
                                log_warn!(
                                    "[app] {e}; serving plain HTTP only, so the phone cannot use \
                                     Bluetooth. The install page will say HTTPS is not running \
                                     rather than blaming the certificate."
                                );
                                (
                                    None,
                                    TlsStatus::Failed {
                                        detail: format!(
                                            "{e}. Another bridge is probably already using port {https_port}."
                                        ),
                                    },
                                )
                            }
                        }
                    }
                    None => (
                        None,
                        TlsStatus::NotConfigured {
                            detail: tls_unavailable
                                .unwrap_or_else(|| "no certificate was created".to_string()),
                        },
                    ),
                };
                // Set from the bind result, never from the intent. A window
                // saying the phone is ready to pair while nothing is listening
                // on 8443 sends the user off to debug their phone, their Wi-Fi
                // and eventually the certificate — everything except the
                // listener that never started.
                if let Ok(mut slot) = state.tls_status.lock() {
                    *slot = tls_state;
                }

                let ctx = Arc::new(http::Ctx {
                    snapshot_rx,
                    cmd_tx,
                    static_dir,
                    library,
                    pairing_base,
                    token: std::sync::RwLock::new(token),
                    allowed_hosts,
                    on_token_rotated: Some(on_token_rotated),
                    tls: https.as_ref().map(|(_, p)| Arc::clone(&p.public)),
                });
                // Published before serving starts, so anything asking for the
                // current pairing URL gets the live token rather than a copy.
                if let Ok(mut slot) = state.http_ctx.lock() {
                    *slot = Some(Arc::clone(&ctx));
                }
                // Only now is the port genuinely ours. Marking it serving any
                // earlier would be the promise-before-confirmation bug this
                // enum exists to prevent.
                if let Ok(mut slot) = state.http_status.lock() {
                    *slot = HttpStatus::Serving;
                }

                // Both listeners share one context and one routing table.
                // Plain HTTP is not a fallback to be retired: it carries the
                // install page, which is the only thing an unpaired phone can
                // reach.
                if let Some((listener, prepared)) = https {
                    log_info!("[app] serving TLS on port {https_port}");
                    let (certs_tx, certs_rx) = tokio::sync::watch::channel(prepared.material);
                    tauri::async_runtime::spawn(tls::keep_current(
                        prepared.ca,
                        certs_tx,
                        advertise,
                    ));
                    tauri::async_runtime::spawn(tls::run(listener, Arc::clone(&ctx), certs_rx));
                }

                http::run(listener, ctx).await;

                // `http::run` loops forever, so reaching here means the
                // listener died under us.
                log_warn!("[app] the phone-facing server stopped");
                if let Ok(mut slot) = state.http_status.lock() {
                    *slot = HttpStatus::Failed {
                        detail: "the server stopped unexpectedly".into(),
                    };
                }
            }
            Err(e) => {
                // Not fatal: the player link is the thing under test, and it
                // works whether or not a phone can reach us. Record it so the
                // window can say the QR will not work rather than showing a
                // QR that leads nowhere.
                let message = format!(
                    "could not serve on {bind}: {e}. If another bridge is already \
                     running, this one cannot take the port."
                );
                log_warn!("[app] {message}");
                if let Ok(mut slot) = state.http_status.lock() {
                    *slot = HttpStatus::Failed { detail: message };
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
