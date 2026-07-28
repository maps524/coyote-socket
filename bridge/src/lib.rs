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
//! - `http` — static serving + WebSocket relay. Tested.
//! - `fake_player` — the stand-in that makes the above testable at all.
//! - `tray`, `qr` — the pairing affordance.

pub mod codec;
pub mod fake_player;
pub mod http;
pub mod logging;
pub mod player;
pub mod qr;
pub mod state;

#[cfg(feature = "tray")]
pub mod tray;
