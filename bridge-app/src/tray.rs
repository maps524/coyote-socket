//! The system tray icon.
//!
//! Built on `tauri::tray` rather than on `tray-icon` directly, unlike the
//! headless binary's tray. Both are the same underlying crate — Tauri 2's tray
//! *is* `tray-icon` — but going through Tauri gets the message pump, the
//! lifetime and the window handle for free, and this crate already has Tauri
//! in it. The library keeps its own standalone tray for the headless binary,
//! where there is no Tauri to borrow from.
//!
//! ## Windows 11 hides new tray icons
//!
//! A freshly registered icon goes into the overflow flyout, not onto the
//! taskbar, and stays there until the user drags it out. Nothing an
//! application can do changes that — it is a user setting
//! (Settings → Personalisation → Taskbar → Other system tray icons), and
//! that is deliberate on Microsoft's part. So the window is shown on launch
//! rather than starting hidden in a tray the user may not be able to see, and
//! the README says where to look.

use tauri::{
    image::Image,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, Runtime,
};

use crate::Urls;

pub fn install<R: Runtime>(app: &AppHandle<R>, urls: &Urls) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show bridge window", true, None::<&str>)?;
    let pair = MenuItem::with_id(app, "pair", "Pairing QR for the phone", true, None::<&str>)?;
    let status = MenuItem::with_id(app, "status", "Bridge status (JSON)", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[&show, &pair, &status, &PredefinedMenuItem::separator(app)?, &quit],
    )?;

    // Pixels come from the library so this tray and the headless one are the
    // same mark.
    let icon = Image::new_owned(
        coyote_bridge::icon::bolt_rgba(),
        coyote_bridge::icon::SIZE,
        coyote_bridge::icon::SIZE,
    );

    let local = urls.local.clone();
    let menu_local = local.clone();

    TrayIconBuilder::with_id("bridge")
        .icon(icon)
        .tooltip(format!("CoyoteSocket bridge — {}", urls.pairing))
        .menu(&menu)
        // Left click should surface the window, not open the menu. Without
        // this, Tauri shows the menu on either button and the click gesture
        // does nothing useful.
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id.as_ref() {
            "show" => surface(app),
            "pair" => open_url(&format!("{menu_local}/pair")),
            "status" => open_url(&format!("{menu_local}/healthz")),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // Act on button-down rather than button-up. When the icon lives in
            // Windows 11's overflow flyout — which is where a new icon always
            // starts — the flyout closes on the press, and the release lands
            // on whatever is underneath instead of on the icon. Matching `Up`
            // meant the click was simply never delivered.
            match event {
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Down,
                    ..
                }
                | TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                } => surface(tray.app_handle()),
                _ => {}
            }
        })
        .build(app)?;

    Ok(())
}

/// Bring the main window back, wherever it went.
///
/// `show` alone is not enough on Windows when the window was minimised, and
/// `set_focus` alone does nothing to a hidden window — closing the window
/// hides it, so both cases are reachable from a single tray click.
fn surface<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// Open `url` in the default browser.
///
/// Notably **not** through `cmd /C start` on Windows, which the first version
/// of this did. `cmd` parses its argument as a command line, so any URL
/// carrying `&`, `|`, `^` or `%` is one quoting mistake away from being
/// executed rather than opened. Callers are supposed to have validated the URL
/// first (see `commands::is_bridge_url`), but a validator and a shell is a
/// combination that only has to be wrong once. Tauri's `opener` hands the
/// string to the OS shell-execute API as a single opaque argument, so there is
/// no command line to escape.
pub fn open_url(url: &str) {
    coyote_bridge::log_info!("[tray] opening {url}");
    if let Err(e) = tauri_plugin_opener::open_url(url, None::<&str>) {
        coyote_bridge::log_warn!("[tray] could not open browser: {e}. Open {url} manually.");
    }
}
