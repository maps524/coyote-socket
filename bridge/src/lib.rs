//! CoyoteSocket bridge — **spike**.
//!
//! A minimal process that sits between a VR video player and a phone browser:
//!
//! ```text
//!   Quest (DeoVR / HereSphere, TCP :23554)
//!      ▲  raw TCP — the bridge dials in
//!   Bridge (this)  ── serves the PWA over HTTP, relays state over WebSocket
//!      ▼  the phone dials in
//!   Phone (PWA)  ──Bluetooth──▶  Coyote
//! ```
//!
//! Exposed as a library so the integration tests can drive the real client
//! against the fake player in-process, rather than testing a subprocess.
//!
//! ## Status of each part
//!
//! - `codec` — framing. Unit-tested, and provenance documented in the module.
//! - `player` — the client. Tested against `fake_player`, **never against a
//!   real headset**. Treat it as a hypothesis until someone runs it against a
//!   Quest.
//! - `supervisor` — connect/disconnect on demand, wrapping `player`'s retry
//!   loop so a UI can drive it.
//! - `probe` — reachability, so "wrong address" and "our framing is wrong"
//!   cannot be mistaken for each other.
//! - `wire` — a live tap on the raw framing. The spike's actual deliverable.
//! - `http` — static serving + WebSocket relay. Tested.
//! - `fake_player` — the stand-in that makes the above testable at all, and
//!   the target to point MultiFunPlayer at for an independent check.
//! - `tray`, `qr` — the pairing affordance.
//!
//! ## No Tauri here, deliberately
//!
//! This crate carries all the protocol, HTTP, WebSocket and state logic and
//! depends on no windowing stack. The desktop UI lives in a separate crate
//! (`bridge-app/`) that depends on this one. The spike record says the
//! bridge's eventual home is an always-on process on a home server that may
//! have no display at all, so `--no-default-features` has to keep producing a
//! working headless build — which it cannot if a window is a compile-time
//! dependency of the protocol.

pub mod capture;
pub mod codec;
pub mod fake_player;
pub mod http;
pub mod icon;
pub mod logging;
pub mod player;
pub mod probe;
pub mod qr;
pub mod state;
pub mod supervisor;
pub mod wire;

#[cfg(feature = "tray")]
pub mod tray;
