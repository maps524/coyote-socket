//! The IPC surface. Thin by design: every one of these is a translation of a
//! button into a library call, and none of them contains protocol logic.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use coyote_bridge::fake_player::{serve, FakePlayerConfig};
use coyote_bridge::probe::{self, Reachability};
use coyote_bridge::state::{LinkState, PlayerCommand, PlayerSnapshot};
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
    /// The full pairing URL with the live token. Derived each call rather than
    /// stored, so revoking a token changes what the window shows.
    pub pairing_url: String,
    /// Whether the phone-facing server is actually listening.
    ///
    /// Three-valued rather than an error-or-nothing, so the window can say
    /// "starting" instead of implying a QR works before anything has bound.
    pub http: crate::HttpStatus,
    /// Whether the phone can get a secure context — what Web Bluetooth needs.
    ///
    /// Same three-plus-one shape as `http`, and set from the TLS bind rather
    /// than from "a certificate was issued". Those differ exactly when another
    /// bridge already holds the port, which is the common case while developing
    /// one.
    pub tls: crate::TlsStatus,
    pub endpoint: String,
    pub recents: Vec<String>,
    pub static_dir: Option<String>,
    pub library_dir: Option<String>,
    pub fake_player: Option<String>,
    pub version: &'static str,
}

#[tauri::command]
pub fn bridge_status(state: Shared) -> Status {
    let settings = state.settings.lock().expect("settings lock");
    Status {
        snapshot: state.bridge.snapshot(),
        urls: state.urls.clone(),
        pairing_url: state.pairing_url(),
        http: state
            .http_status
            .lock()
            .map(|s| s.clone())
            .unwrap_or(crate::HttpStatus::Starting),
        tls: state
            .tls_status
            .lock()
            .map(|s| s.clone())
            .unwrap_or(crate::TlsStatus::Starting),
        endpoint: settings.endpoint.clone(),
        recents: settings.recents.clone(),
        static_dir: settings.static_dir.clone(),
        library_dir: settings.library_dir.clone(),
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

    // Skip the probe when this endpoint is already up. Probing opens a second
    // connection, and a real player that accepts one client at a time may drop
    // the live one to take it — so the check would break exactly what it is
    // meant to verify.
    let already_connected = {
        let snapshot = state.bridge.snapshot();
        snapshot.endpoint == endpoint && snapshot.link == LinkState::Connected
    };
    let reachability = if already_connected {
        Reachability::PortOpen
    } else {
        probe::probe(&endpoint).await
    };
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
    qr::to_svg(&state.pairing_url()).map_err(|e| e.to_string())
}

/// Revoke the current pairing token and issue a new one.
///
/// Deliberately a user action rather than something the pairing flow does on
/// its own. There is one shared token, so this un-pairs **every** device at
/// once — the phone, a tablet, a desktop browser tab. Doing it automatically
/// after a phone paired would silently break every other device, which is a
/// larger and far more likely harm than the exposure it would close.
///
/// The situations this is actually for: a QR that was shown to someone, or a
/// URL that was pasted somewhere it should not have been.
#[tauri::command]
pub fn rotate_token(state: Shared) -> Result<String, String> {
    let http = state
        .http_ctx
        .lock()
        .map_err(|_| "state lock poisoned")?
        .clone()
        .ok_or("the phone-facing server is not running, so there is no token to rotate")?;

    http.rotate_token();
    log_info!("[app] pairing token revoked by the user; every paired device must re-scan");
    // The QR is derived from the token, so it changes with it.
    qr::to_svg(&state.pairing_url()).map_err(|e| e.to_string())
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

/// Point the bridge at a directory of funscripts.
///
/// **Takes effect on the next launch**, for the same reason as
/// `set_static_dir`: the library's poller is started when the server starts.
/// Callers must say so, because until then `bridge_status` reports the new path
/// while `/library/index.json` still answers for the old one — two sources of
/// truth for the same question, which is exactly the shape `FOLLOW-UPS.md` §0
/// is about. There is no UI for this yet, which is what keeps it harmless;
/// whoever builds one has to either restart the poller here or label the field.
///
/// A path that does not exist yet is **accepted**, deliberately. `--library-dir`
/// accepts one and `Library::spawn` tolerates one so a network share can mount
/// after login, and a command that refused what the CLI allows would be a
/// second, stricter answer to the same question. A wrong path is no longer
/// silent either way: the index reports `scan: "failed"` rather than an empty
/// listing.
#[tauri::command]
pub fn set_library_dir(path: Option<String>, state: Shared) -> Result<(), String> {
    let path = path.filter(|p| !p.trim().is_empty());
    if let Some(p) = &path {
        if !std::path::Path::new(p).is_dir() {
            log_warn!("[app] library directory {p} does not exist yet; it will be polled for");
        }
    }
    state
        .settings
        .lock()
        .map_err(|_| "state lock poisoned")?
        .library_dir = path;
    state.save_settings();
    Ok(())
}

/// Open a URL in the default browser.
///
/// Restricted to our own bridge URLs. The window is the only caller today, but
/// this crate's whole purpose is serving pages to a LAN, so an "open anything"
/// command is a door worth closing before someone walks through it.
#[tauri::command]
pub fn open_external(url: String, state: Shared) -> Result<(), String> {
    if !is_bridge_url(&url, &[&state.urls.local, &state.urls.pairing_base]) {
        return Err(format!("refusing to open {url}: not a bridge URL"));
    }
    crate::tray::open_url(&url);
    Ok(())
}

/// Whether `url` is one of ours.
///
/// A bare `starts_with` is not enough, and the gap is not theoretical: the URL
/// reaches `cmd /C start`, so `http://192.168.0.5:8787@evil/x&calc.exe` passes
/// a prefix test, is a legal URL whose *host* is `evil`, and carries a shell
/// metacharacter into a command line. Requiring a delimiter after the base
/// closes the host-confusion half; [`crate::tray::open_url`] no longer goes
/// through `cmd` at all, which closes the other.
fn is_bridge_url(url: &str, bases: &[&String]) -> bool {
    bases.iter().any(|base| {
        let Some(rest) = url.strip_prefix(base.as_str()) else {
            return false;
        };
        // The authority must end here. Anything that continues it — `@`, a
        // digit extending the port, another label — is a different origin
        // wearing our prefix.
        matches!(rest.chars().next(), None | Some('/') | Some('?') | Some('#'))
    })
}

/// Nothing here should ever be reached with a poisoned lock, but a warn beats
/// a panic in a process someone is mid-test with.
#[allow(dead_code)]
fn warn_poisoned(what: &str) {
    log_warn!("[app] {what} lock was poisoned");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bases() -> Vec<String> {
        vec![
            "http://127.0.0.1:8787".to_string(),
            "http://192.168.0.5:8787".to_string(),
        ]
    }

    #[test]
    fn our_own_urls_are_allowed() {
        let b = bases();
        let refs: Vec<&String> = b.iter().collect();
        for url in [
            "http://127.0.0.1:8787",
            "http://127.0.0.1:8787/",
            "http://127.0.0.1:8787/pair",
            "http://192.168.0.5:8787/healthz?pretty=1",
            "http://192.168.0.5:8787/pair#qr",
        ] {
            assert!(is_bridge_url(url, &refs), "should allow {url}");
        }
    }

    /// The bypass this check exists for. Each of these passes a bare
    /// `starts_with` and none of them is our origin.
    #[test]
    fn a_prefix_is_not_an_origin() {
        let b = bases();
        let refs: Vec<&String> = b.iter().collect();
        for url in [
            // userinfo trick: the real host is `x`, and `&calc.exe` was a
            // shell metacharacter on the old `cmd /C start` path.
            "http://192.168.0.5:8787@x&calc.exe",
            // port extension: :87879 is not :8787.
            "http://192.168.0.5:87879/evil",
            // label extension: a different host entirely.
            "http://127.0.0.1:8787.evil.com/",
            "https://127.0.0.1:8787/",
            "file:///C:/Windows/System32/calc.exe",
        ] {
            assert!(!is_bridge_url(url, &refs), "should refuse {url}");
        }
    }
}
