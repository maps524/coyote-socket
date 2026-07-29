//! A tap on the raw TCP framing, so a mismatch is *legible* rather than just
//! fatal.
//!
//! The spike's deliverable was always "the log" — every payload a real player
//! sends, before we interpret it. A log file is a poor place to read that from
//! while a headset is in the room, so the same events are also published on a
//! broadcast channel that a UI can render live.
//!
//! What each event carries is chosen for one purpose: telling a **framing**
//! failure apart from a **semantic** one.
//!
//! - `prefix_hex` is the four length bytes exactly as they came off the wire.
//!   If our byte order were wrong, this is where it shows: `2c 01 00 00` is a
//!   300-byte packet; `00 00 01 2c` is the same packet from a peer that
//!   disagrees with us.
//! - `len` is what *we* decoded that prefix as. Printed next to the bytes, the
//!   two together are self-checking.
//! - `note` carries a diagnosis when the numbers look wrong (see
//!   [`crate::codec::diagnose_prefix`]).
//!
//! The tap is cheap and optional. `Tap::disabled()` compiles to a no-op branch,
//! which is what the headless binary and the tests use.

use serde::Serialize;
use tokio::sync::broadcast;

/// Which link an event was observed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Our client's socket to the player. This is the hypothesis under test.
    Player,
    /// The built-in fake player's socket to whatever connected to it —
    /// MultiFunPlayer, during a cross-check run.
    Fake,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Dir {
    /// Received by the observer.
    In,
    /// Sent by the observer.
    Out,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Heartbeat,
    Json,
    /// Framing broke, or the payload was not what the framing promised.
    Error,
    /// Not a frame — a connection-level event worth showing inline with the
    /// frames, so the log reads as one story.
    Event,
}

/// One observed frame.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireEvent {
    /// Monotonically increasing per process. Lets a UI dedupe and order without
    /// trusting the clock.
    pub seq: u64,
    pub at_ms: u64,
    pub source: Source,
    pub dir: Dir,
    pub kind: Kind,
    /// The four length bytes, space-separated hex. Empty for `Event`.
    pub prefix_hex: String,
    /// Length as *we* decoded it (little-endian, signed).
    pub len: i64,
    pub text: Option<String>,
    /// A plain-language diagnosis when something looks off.
    pub note: Option<String>,
}

/// A clonable handle for publishing wire events.
///
/// Deliberately swallows send errors: nothing on this path may fail because
/// nobody happens to be watching.
#[derive(Clone)]
pub struct Tap {
    tx: Option<broadcast::Sender<WireEvent>>,
    counter: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl Tap {
    /// A tap nobody is listening to. Emitting on it costs a branch.
    pub fn disabled() -> Self {
        Self {
            tx: None,
            counter: Default::default(),
        }
    }

    /// A live tap plus its first receiver.
    ///
    /// `capacity` bounds the backlog; a slow UI lags rather than blocking the
    /// protocol, and `broadcast` tells it how many it missed.
    pub fn channel(capacity: usize) -> (Self, broadcast::Receiver<WireEvent>) {
        let (tx, rx) = broadcast::channel(capacity);
        (
            Self {
                tx: Some(tx),
                counter: Default::default(),
            },
            rx,
        )
    }

    pub fn subscribe(&self) -> Option<broadcast::Receiver<WireEvent>> {
        self.tx.as_ref().map(|tx| tx.subscribe())
    }

    pub fn is_enabled(&self) -> bool {
        self.tx.is_some()
    }

    /// Publish a framed event.
    pub fn frame(
        &self,
        source: Source,
        dir: Dir,
        kind: Kind,
        prefix: [u8; 4],
        len: i64,
        text: Option<String>,
        note: Option<String>,
    ) {
        let Some(tx) = &self.tx else { return };
        let seq = self
            .counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let _ = tx.send(WireEvent {
            seq,
            at_ms: crate::logging::now_ms(),
            source,
            dir,
            kind,
            prefix_hex: hex4(prefix),
            len,
            text,
            note,
        });
    }

    /// Publish a connection-level note — "connected", "refused", "dropped".
    pub fn event(&self, source: Source, text: impl Into<String>) {
        let Some(tx) = &self.tx else { return };
        let seq = self
            .counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let _ = tx.send(WireEvent {
            seq,
            at_ms: crate::logging::now_ms(),
            source,
            dir: Dir::In,
            kind: Kind::Event,
            prefix_hex: String::new(),
            len: 0,
            text: Some(text.into()),
            note: None,
        });
    }
}

impl Default for Tap {
    fn default() -> Self {
        Self::disabled()
    }
}

/// `[0x2c, 0x01, 0x00, 0x00]` becomes `"2c 01 00 00"`.
pub fn hex4(bytes: [u8; 4]) -> String {
    format!(
        "{:02x} {:02x} {:02x} {:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_is_wire_order_not_numeric_order() {
        // 300 little-endian. Printed as it arrived, so it can be compared with
        // a packet capture byte for byte.
        assert_eq!(hex4(300i32.to_le_bytes()), "2c 01 00 00");
    }

    #[tokio::test]
    async fn a_disabled_tap_is_inert() {
        let tap = Tap::disabled();
        tap.event(Source::Player, "nobody hears this");
        assert!(!tap.is_enabled());
        assert!(tap.subscribe().is_none());
    }

    #[tokio::test]
    async fn events_are_sequenced() {
        let (tap, mut rx) = Tap::channel(8);
        tap.event(Source::Player, "one");
        tap.frame(
            Source::Player,
            Dir::In,
            Kind::Json,
            [2, 0, 0, 0],
            2,
            Some("{}".into()),
            None,
        );
        assert_eq!(rx.recv().await.unwrap().seq, 0);
        let second = rx.recv().await.unwrap();
        assert_eq!(second.seq, 1);
        assert_eq!(second.prefix_hex, "02 00 00 00");
    }
}
