//! System tray icon: click it, get the QR the phone should scan.
//!
//! Built on `tao` + `tray-icon` directly rather than on Tauri. Those are the
//! exact crates Tauri 2's own tray support is built from, so folding this back
//! into `src-tauri` later is a matter of swapping `TrayIconBuilder` for
//! `tauri::tray::TrayIconBuilder` — the icon, the menu and the click handling
//! carry over. Going through Tauri here would have meant a second
//! `tauri.conf.json`, a second icon set and a second frontend build in a repo
//! that already has one of each, for no additional capability.
//!
//! The QR itself is a page the bridge already serves (`/pair`), opened in the
//! default browser. That means no webview, and it also means the pairing page
//! is reachable from any machine on the network, not only from the tray.

use std::sync::mpsc;

use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    Icon, TrayIconBuilder, TrayIconEvent,
};

use crate::{log_info, log_warn};

/// Run the tray on the calling thread and never return.
///
/// Must be called from the process's main thread: on Windows the tray icon
/// needs a message pump on the thread that created it, and on macOS the event
/// loop is main-thread-only.
pub fn run(pairing_url: String, local_url: String, status_url: String) -> ! {
    let event_loop = EventLoopBuilder::new().build();

    let menu = Menu::new();
    let show = MenuItem::new("Show phone URL (QR)", true, None);
    let status = MenuItem::new("Bridge status", true, None);
    let quit = MenuItem::new("Quit", true, None);
    let _ = menu.append_items(&[&show, &status, &PredefinedMenuItem::separator(), &quit]);

    let show_id = show.id().clone();
    let status_id = status.id().clone();
    let quit_id = quit.id().clone();

    let _tray = match TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip(format!(
            "CoyoteSocket bridge — {}",
            crate::auth::redact_url(&pairing_url)
        ))
        .with_icon(bolt_icon())
        .build()
    {
        Ok(tray) => Some(tray),
        Err(e) => {
            // A headless box has no tray. That is not fatal — the bridge is
            // fully functional without one.
            log_warn!("[tray] could not create tray icon ({e}); running without it");
            None
        }
    };

    // Channel the two event sources into one place so the tao closure stays
    // small; both `set_event_handler` callbacks fire on arbitrary threads.
    let (tx, rx) = mpsc::channel::<Action>();

    {
        let tx = tx.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let action = if event.id == show_id {
                Action::ShowPairing
            } else if event.id == status_id {
                Action::ShowStatus
            } else if event.id == quit_id {
                Action::Quit
            } else {
                return;
            };
            let _ = tx.send(action);
        }));
    }
    {
        let tx = tx.clone();
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            // A plain left click is the obvious gesture for "show me the QR".
            if let TrayIconEvent::Click { button, .. } = event {
                if button == tray_icon::MouseButton::Left {
                    let _ = tx.send(Action::ShowPairing);
                }
            }
        }));
    }

    log_info!("[tray] ready — click the tray icon for the pairing QR");

    event_loop.run(move |_event, _target, control_flow| {
        *control_flow = ControlFlow::Wait;

        while let Ok(action) = rx.try_recv() {
            match action {
                Action::ShowPairing => open_url(&format!("{local_url}/pair")),
                // Carries the token: /healthz is gated, so a bare URL would 401.
                Action::ShowStatus => open_url(&status_url),
                Action::Quit => {
                    log_info!("[tray] quit requested");
                    *control_flow = ControlFlow::Exit;
                }
            }
        }
    })
}

enum Action {
    ShowPairing,
    ShowStatus,
    Quit,
}

/// Open `url` in the default browser without pulling in a crate for it.
fn open_url(url: &str) {
    // Redacted: the tray can be asked to open URLs that carry the token, and
    // this log is routinely shared.
    log_info!("[tray] opening {}", crate::auth::redact_url(url));
    let result = if cfg!(target_os = "windows") {
        // Not `cmd /C start`. `cmd` re-parses its argument as a command line,
        // so `&`, `|` and `^` in a URL become shell operators — one quoting
        // mistake from executing rather than opening. `rundll32` receives the
        // URL as a plain argument with no shell in the path. (The Tauri build
        // uses the `opener` plugin; this crate has no such dependency and is
        // not about to grow one for a tray.)
        std::process::Command::new("rundll32.exe")
            .args(["url.dll,FileProtocolHandler", url])
            .spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(url).spawn()
    };

    if let Err(e) = result {
        // Redacted here too: an error path is exactly when someone copies the
        // log.
        log_warn!(
            "[tray] could not open browser: {e}. Open {} manually.",
            crate::auth::redact_url(url)
        );
    }
}

/// The shared bolt, as `tray-icon` wants it. The pixels come from
/// [`crate::icon`] so this tray and the Tauri app's tray are the same mark.
fn bolt_icon() -> Icon {
    Icon::from_rgba(crate::icon::bolt_rgba(), crate::icon::SIZE, crate::icon::SIZE)
        .expect("bolt icon buffer is 32x32 RGBA by construction")
}
