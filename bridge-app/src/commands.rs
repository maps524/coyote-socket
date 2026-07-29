//! The IPC surface. Thin by design: every one of these is a translation of a
//! button into a library call, and none of them contains protocol logic.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use coyote_bridge::fake_player::{serve, FakePlayerConfig};
use coyote_bridge::probe::{self, Reachability};
use coyote_bridge::state::{PlayerCommand, PlayerSnapshot};
use coyote_bridge::{log_info, log_warn, logging, qr};
use serde::Serialize;
use tauri::State;
use tokio::net::TcpListener;

use crate::{AppState, FakePlayer, Urls};

/// The well-known DeoVR / HereSphere remote-control port.
const PLAYER_PORT: u16 = 23554;

type Shared<'a> = State<'a, Arc<AppState>>;

/// Everything the window needs to render itself on load, in one call.
///
/// One round trip rather than six because the frontend renders this on mount
/// and again after every action, and a half-populated window during the gaps
/// looks like a bug.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub snapshot: PlayerSnapshot,
    pub urls: Urls,
    /// Non-null when the phone-facing server failed to start; the QR will not
    /// work and the window should say so.
    pub http_error: Option<String>,
    pub endpoint: String,
    pub recents: Vec<String>,
    pub static_dir: Option<String>,
    pub fake_player: Option<String>,
    pub version: &'static str,
}

#[tauri::command]
pub fn bridge_status(state: Shared) -> Status {
    let settings = state.settings.lock().expect("settings lock");
    Status {
        snapshot: state.bridge.snapshot(),
        urls: state.urls.clone(),
        http_error: state.http_error.lock().ok().and_then(|e| e.clone()),
        endpoint: settings.endpoint.clone(),
        recents: settings.recents.clone(),
        static_dir: settings.static_dir.clone(),
        fake_player: state
            .fake_player
            .lock()
            .ok()
            .and_then(|f| f.as_ref().map(|f| f.endpoint.clone())),
        version: env!("CARGO_PKG_VERSION"),
    }
}

/// Check an address without committing to it.
///
/// Separate from `connect` so the window can answer "is this the right IP?"
/// before anyone waits on a retry loop, and so a failure can be attributed to
/// the network rather than to the protocol. That attribution is the entire
/// value of the headset test.
#[tauri::command]
pub async fn check_reachability(endpoint: String) -> Result<Reachability, String> {
    let endpoint = probe::normalise_endpoint(&endpoint, PLAYER_PORT);
    if !probe::looks_like_an_endpoint(&endpoint) {
        return Err(format!("{endpoint:?} is not an address and a port"));
    }
    log_info!("[app] probing {endpoint}");
    Ok(probe::probe(&endpoint).await)
}

/// Connect, after a probe.
///
/// The probe runs first so that the common failures — wrong address, remote
/// control switched off — are named immediately instead of arriving as a
/// generic retry after a timeout. When the probe finds the port open we do not
/// hold that connection: real players accept one client at a time, so the
/// probe socket is dropped and the supervisor opens the real one.
#[tauri::command]
pub async fn connect(endpoint: String, state: Shared<'_>) -> Result<Reachability, String> {
    let endpoint = probe::normalise_endpoint(&endpoint, PLAYER_PORT);
    if !probe::looks_like_an_endpoint(&endpoint) {
        return Err(format!(
            "{endpoint:?} is not an address and a port. Use the headset's IP, \
             optionally followed by :{PLAYER_PORT}."
        ));
    }

    let reachability = probe::probe(&endpoint).await;
    log_info!("[app] connect {endpoint}: {}", reachability.summary());

    // Remember it either way. A typo is worth forgetting, but an address that
    // was merely asleep is worth keeping — and we cannot tell those apart.
    if let Ok(mut settings) = state.settings.lock() {
        settings.remember(&endpoint);
    }
    state.save_settings();

    // Connect even when the probe was unhappy: the supervisor's backoff is the
    // right place to wait for a headset that is booting, and refusing to try
    // would make "start the player, then press Connect again" the only
    // workflow. The probe result is returned so the window can explain the
    // wait rather than just spinning.
    state.bridge.connect(endpoint).await?;
    Ok(reachability)
}

#[tauri::command]
pub async fn disconnect(state: Shared<'_>) -> Result<(), String> {
    state.bridge.disconnect().await
}

/// Drive the player from the desktop. Same three commands the phone gets.
///
/// Worth having in the window because it is the cheapest available check that
/// our *outbound* framing is right: if a seek moves the video in the headset,
/// the player parsed something we wrote.
#[tauri::command]
pub async fn send_player_command(
    kind: String,
    position_s: Option<f64>,
    state: Shared<'_>,
) -> Result<(), String> {
    let command = match kind.as_str() {
        "seek" => PlayerCommand {
            current_time: Some(position_s.ok_or("seek needs a position")?),
            ..Default::default()
        },
        // 0 = playing, 1 = paused, per the player protocol.
        "play" => PlayerCommand {
            player_state: Some(0),
            ..Default::default()
        },
        "pause" => PlayerCommand {
            player_state: Some(1),
            ..Default::default()
        },
        other => return Err(format!("unknown command {other:?}")),
    };
    state
        .bridge
        .cmd_tx
        .send(command)
        .await
        .map_err(|_| "the player link is not running".to_string())
}

/// The pairing QR as an SVG document, rendered server-side.
#[tauri::command]
pub fn pairing_qr(state: Shared) -> Result<String, String> {
    qr::to_svg(&state.urls.pairing).map_err(|e| e.to_string())
}

/// The whole ring buffer, for the copy button.
#[tauri::command]
pub fn log_history() -> Vec<String> {
    logging::history()
}

/// Start the built-in fake player, so MultiFunPlayer can be pointed at it.
///
/// In-process rather than as a spawned `fake-player` binary: the frames it
/// sees go straight onto the same wire tap the window is already rendering,
/// which is the whole reason this is worth having in the app. A subprocess
/// would put the interesting evidence in a second console.
///
/// Binds all interfaces so MFP can be on another machine.
#[tauri::command]
pub async fn start_fake_player(port: Option<u16>, state: Shared<'_>) -> Result<String, String> {
    if state
        .fake_player
        .lock()
        .map_err(|_| "state lock poisoned")?
        .is_some()
    {
        return Err("the test player is already running".into());
    }

    let port = port.unwrap_or(PLAYER_PORT);
    let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port);
    let listener = TcpListener::bind(bind).await.map_err(|e| {
        format!(
            "could not listen on {bind}: {e}. If a real player or another copy \
             of this app already has {port}, stop that first."
        )
    })?;

    let tap = state.bridge.tap.clone();
    let task = tauri::async_runtime::spawn(async move {
        serve(listener, FakePlayerConfig::default(), tap).await;
    });

    let endpoint = format!("127.0.0.1:{port}");
    log_info!("[app] test player listening on {bind}");
    *state.fake_player.lock().map_err(|_| "state lock poisoned")? = Some(FakePlayer {
        task,
        endpoint: endpoint.clone(),
    });
    Ok(endpoint)
}

#[tauri::command]
pub fn stop_fake_player(state: Shared) -> Result<(), String> {
    let mut slot = state.fake_player.lock().map_err(|_| "state lock poisoned")?;
    if let Some(fake) = slot.take() {
        fake.task.abort();
        log_info!("[app] test player stopped");
    }
    Ok(())
}

/// Point the bridge at a built PWA. Takes effect on the next launch, because
/// the static root is captured when the server starts.
#[tauri::command]
pub fn set_static_dir(path: Option<String>, state: Shared) -> Result<(), String> {
    let path = path.filter(|p| !p.trim().is_empty());
    if let Some(p) = &path {
        if !std::path::Path::new(p).is_dir() {
            return Err(format!("{p} is not a directory"));
        }
    }
    state
        .settings
        .lock()
        .map_err(|_| "state lock poisoned")?
        .static_dir = path;
    state.save_settings();
    Ok(())
}

/// Open a URL in the default browser.
///
/// Restricted to our own bridge URLs. The window is the only caller, but an
/// unrestricted "open anything" command is a wide door to leave in an app that
/// serves pages to a LAN.
#[tauri::command]
pub fn open_external(url: String, state: Shared) -> Result<(), String> {
    let allowed = [&state.urls.local, &state.urls.pairing];
    if !allowed.iter().any(|base| url.starts_with(base.as_str())) {
        return Err(format!("refusing to open {url}: not a bridge URL"));
    }
    crate::tray::open_url(&url);
    Ok(())
}

/// Nothing here should ever be reached with a poisoned lock, but a warn beats
/// a panic in a process someone is mid-test with.
#[allow(dead_code)]
fn warn_poisoned(what: &str) {
    log_warn!("[app] {what} lock was poisoned");
}
