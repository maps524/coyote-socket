//! Recorded traffic from a real player, and what it told us.
//!
//! # Why this file exists
//!
//! Until 2026-07-28 every test in this repo was circular. The client was
//! written from DeoVR's published documentation and MultiFunPlayer's adapters;
//! `fake_player` was written from the same two documents by the same reading;
//! and the client was then tested against the fake. A shared misreading would
//! have passed every one of those tests.
//!
//! On 2026-07-28 the bridge connected to DeoVR on a real Quest and its wire tap
//! recorded **837.9 seconds across three connections** — 418 inbound JSON
//! frames, 21 outbound commands, 9 link events. That capture is committed raw
//! at `fixtures/deovr-quest-2026-07-28.wire.jsonl` and every number below is
//! re-derived from it by the tests in this module.
//!
//! # Framing is now observed, not reasoned
//!
//! The capture records the four length bytes **before decoding**. For all 418
//! inbound frames, the prefix read little-endian equals the payload's UTF-8
//! byte count:
//!
//! ```text
//!   prefix  c6 00 00 00
//!   little-endian ->            198   payload is 198 UTF-8 bytes  ok
//!   big-endian    -> 3 321 888 768   absurd
//! ```
//!
//! That is the difference between evidence and inference. Byte order was
//! previously argued from MultiFunPlayer calling `BitConverter` on a
//! little-endian host — sound reasoning about a third party's code, but still
//! reasoning. It is now a measurement.
//!
//! It also matters that this fixture is the *raw tap* rather than a
//! reconstruction. An earlier version of this file was built from `log_debug!`
//! of the already-decoded string, so replaying it re-framed each payload with
//! our own encoder. That could only ever show that our decoder agrees with our
//! encoder. Replay now emits the recorded prefix bytes verbatim.
//!
//! # What the capture settles, and what it does not
//!
//! Settled, for DeoVR on this Quest:
//!
//! - Framing: little-endian 4-byte length, UTF-8 JSON. 418 of 418.
//! - The five documented fields are always present. Across all 418 frames the
//!   key set was **exactly** `path`, `duration`, `currentTime`,
//!   `playbackSpeed`, `playerState` — nothing else, ever.
//! - Cadence is ~1006 ms (median).
//! - Our 1 Hz keepalive holds a real player: one connection ran 215.9 s.
//! - **The stream does not stall on a live connection.** Zero gaps over two
//!   seconds occurred within a connection. Every large gap was a reconnect.
//!
//! Not settled, and not even touched:
//!
//! - **HereSphere.** Never connected to. The claim that one adapter covers both
//!   players remains an inference from MFP's two source files being identical.
//! - **Media changes.** One file for the whole capture.
//! - **On-device media.** The path was always a URL (see below).
//! - **`resource` / `identifier`.** Never seen — they were always HereSphere
//!   fallbacks.
//!
//! # Divergences from what `fake_player` was pretending to be
//!
//! Each is a place the fake was wrong, and therefore a place the tests built on
//! it proved less than they appeared to.
//!
//! | Behaviour | `fake_player` before | Real DeoVR |
//! |---|---|---|
//! | Packet shape | full every 8th tick, `currentTime`-only between | **full every time, 418/418** |
//! | Cadence | 500 ms | **~1006 ms** |
//! | Heartbeat frames | interleaved every 4th tick | **none, in 837 s** |
//! | `path` | `C:\VR\fake-clip.mp4` | **an `http://` URL** |
//!
//! The first is the most consequential. `PlayerSnapshot::apply` merges rather
//! than replaces precisely so a `currentTime`-only update cannot wipe the
//! duration — and the real player never sends one. That logic is still correct
//! and still worth keeping (HereSphere is unobserved, and DeoVR may send
//! partials while scrubbing, which this capture does not show), but it was
//! never exercised by anything real.
//!
//! # `playerState` is advisory, not a status
//!
//! The documented polarity is **correct**: `0` is playing, `1` is paused. Every
//! clean static run in the capture reports `1`, and every clean `0` run
//! advances at 1.0x:
//!
//! ```text
//!   conn1 t+ 59.5s  state 1   n=  4    2.0s  rate 0.005   STATIC
//!   conn1 t+ 61.5s  state 0   n=  4    2.0s  rate 1.025   ADVANCING
//!   conn1 t+ 63.5s  state 1   n=  5    4.0s  rate 0.001   STATIC
//!   conn1 t+ 67.6s  state 0   n= 10    8.8s  rate 1.029   ADVANCING
//!   conn1 t+ 76.5s  state 1   n=140  140.0s  rate 0.887   ADVANCING  <- 1, and playing
//!   conn3 t+651.8s  state 1   n=  8    7.1s  rate 0.001   STATIC
//!   conn3 t+658.9s  state 0   n=  6    5.1s  rate 1.017   ADVANCING
//!   conn3 t+664.7s  state 1   n=  5    4.0s  rate 0.004   STATIC
//! ```
//!
//! Nothing here justifies changing the mapping, and nothing has been changed.
//!
//! **But the field does not observe the player.** The fifth row is 140 frames —
//! well over two minutes — reporting `1` while position advanced. The last
//! thing the bridge sent before it was `{"playerState":1}`, at t+76.0 s. The
//! behaviour is consistent with `playerState` **echoing the last value a remote
//! client set** rather than reporting what the player is doing, so a play or
//! pause performed inside the headset never reaches the field.
//!
//! The outbound commands make the same point directly. Sending `playerState=1`
//! at t+58.4 s was followed by the player still reporting `0`; sending `0` at
//! t+61.2 s was followed by it reporting `1`. The field and the command do not
//! round-trip reliably.
//!
//! Aggregated over the whole capture, excluding frames near our own seeks:
//!
//! ```text
//!   state 0:  n= 67   advancing 61   static  3
//!   state 1:  n=321   advancing 256  static 64
//! ```
//!
//! `0` almost always means playing. `1` means nothing on its own.
//!
//! Two consequences, both applied:
//!
//! 1. **Do not trust `playerState` alone.** Whether position is advancing is
//!    the reliable signal, and it is the one a script sampler should use.
//!    [`PlayerSnapshot::state_is_suspect`] raises exactly this contradiction.
//! 2. **Command and status are separate mappings.** Driving the player with
//!    `playerState` works — play, pause, forward and back all did the right
//!    thing on a real Quest. That says nothing about the status field.
//!
//! # Reconnects are the only discontinuity
//!
//! Three connections, two of them ended by the headset sleeping
//! (`WSAECONNRESET`), one by the user pressing Disconnect. Between them:
//!
//! ```text
//!   gap  59.2s   pos 275.88 -> 278.34   (+2.46)      reconnect
//!   gap 371.0s   pos 283.38 -> 151.66   (-131.72)    reconnect
//! ```
//!
//! DeoVR does **not** keep playing on a sleeping headset: 59 s of silence moved
//! position 2.46 s, and 371 s of silence moved it *backwards* 131.7 s. And
//! critically, there were **zero** in-connection stalls over two seconds. So:
//!
//! > A reconnect is a discontinuity. A live connection is continuous.
//!
//! The first packet after a reconnect is shape-identical to steady state — no
//! handshake, no marker — so the reconnect itself is the only signal, which is
//! why [`PlayerSnapshot::epoch`] exists.
//!
//! # `path` has at least two shapes
//!
//! The captured `path` is an HTTP URL because the media was streamed from a
//! DLNA server (Universal Media Server). A file on the headset presumably
//! reports a filesystem path, but **that form has not been observed** and
//! should not be assumed.
//!
//! For anything matching scripts by name, the useful detail is that the URL is
//! **not opaque** — the final segment is the real filename. So name-based
//! matching remains viable for streamed sources with this server, provided the
//! consumer takes the last segment, percent-decodes it, and drops any query
//! string. Some DLNA servers expose opaque content ids; for those, name
//! matching cannot work at all.

use serde::Deserialize;

/// The capture, embedded so tests need no fixture path and no I/O.
const DEOVR_QUEST: &str = include_str!("../fixtures/deovr-quest-2026-07-28.wire.jsonl");

/// One recorded wire event, in the tap's own JSON Lines format.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturedEvent {
    pub at_ms: u64,
    pub source: String,
    pub dir: String,
    pub kind: String,
    /// The four length bytes as they arrived, space-separated hex. Empty for
    /// link events, which have no frame.
    pub prefix_hex: String,
    pub len: i64,
    pub text: Option<String>,
}

impl CapturedEvent {
    pub fn is_inbound_json(&self) -> bool {
        self.source == "player" && self.dir == "in" && self.kind == "json"
    }

    /// The recorded prefix as bytes, when there is one.
    pub fn prefix_bytes(&self) -> Option<[u8; 4]> {
        let parts: Vec<&str> = self.prefix_hex.split_whitespace().collect();
        if parts.len() != 4 {
            return None;
        }
        let mut out = [0u8; 4];
        for (slot, part) in out.iter_mut().zip(parts) {
            *slot = u8::from_str_radix(part, 16).ok()?;
        }
        Some(out)
    }
}

#[derive(Debug, Clone)]
pub struct Capture {
    pub events: Vec<CapturedEvent>,
}

impl Capture {
    /// The DeoVR-on-a-Quest capture, every recorded event.
    pub fn deovr_quest() -> Self {
        let events = DEOVR_QUEST
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("the embedded capture is valid JSONL"))
            .collect();
        Self { events }
    }

    /// Just the frames the player sent us.
    pub fn inbound(&self) -> Vec<&CapturedEvent> {
        self.events.iter().filter(|e| e.is_inbound_json()).collect()
    }

    /// Inbound frames belonging to the longest single connection.
    ///
    /// Replay uses this rather than the whole file. Replaying across a
    /// reconnect boundary would inject the recorded 131-second backwards jump
    /// into a stream the client is told is continuous — manufacturing, inside
    /// our own test fixture, exactly the discontinuity the rest of the system
    /// is built to treat as impossible on a live link.
    pub fn longest_connection(&self) -> Vec<&CapturedEvent> {
        let mut best: Vec<&CapturedEvent> = Vec::new();
        let mut current: Vec<&CapturedEvent> = Vec::new();
        for event in &self.events {
            let is_link_change = event.kind == "event"
                && event
                    .text
                    .as_deref()
                    .is_some_and(|t| t.contains("connected to") || t.contains("disconnected"));
            if is_link_change {
                if current.len() > best.len() {
                    best = std::mem::take(&mut current);
                }
                current.clear();
                continue;
            }
            if event.is_inbound_json() {
                current.push(event);
            }
        }
        if current.len() > best.len() {
            best = current;
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{PlayerPacket, PlayerSnapshot};
    use std::collections::BTreeSet;

    fn packets(events: &[&CapturedEvent]) -> Vec<(u64, PlayerPacket)> {
        events
            .iter()
            .map(|e| {
                let json = e.text.as_deref().expect("a json frame has text");
                (
                    e.at_ms,
                    serde_json::from_str(json)
                        .unwrap_or_else(|err| panic!("real frame failed to parse: {err}\n{json}")),
                )
            })
            .collect()
    }

    #[test]
    fn the_capture_is_the_size_the_documentation_claims() {
        // Every empirical claim in the module docs and the README is derived
        // from these counts. If the fixture is ever replaced, this fails first
        // and the prose gets revisited rather than quietly becoming fiction.
        let capture = Capture::deovr_quest();
        assert_eq!(capture.events.len(), 450, "total recorded events");
        assert_eq!(capture.inbound().len(), 418, "inbound JSON frames");

        let span = capture.events.last().unwrap().at_ms - capture.events[0].at_ms;
        assert!(
            (837_000..839_000).contains(&span),
            "capture spans 837.9s, got {span}ms"
        );

        let connects = capture
            .events
            .iter()
            .filter(|e| e.text.as_deref().is_some_and(|t| t.contains("connected to")))
            .count();
        assert_eq!(connects, 3, "three connections");
    }

    /// **The non-circular test.** Real bytes off a real player, checked against
    /// our byte-order assumption without our encoder in the loop.
    #[test]
    fn every_recorded_prefix_is_little_endian() {
        let capture = Capture::deovr_quest();
        let inbound = capture.inbound();
        assert!(!inbound.is_empty());

        for event in &inbound {
            let prefix = event.prefix_bytes().expect("an inbound frame has a prefix");
            let payload_bytes = event.text.as_deref().unwrap().len();
            let le = i32::from_le_bytes(prefix) as usize;
            assert_eq!(
                le, payload_bytes,
                "prefix {} decodes little-endian to {le} but the payload is {payload_bytes} bytes",
                event.prefix_hex
            );
        }
    }

    /// The same evidence stated as a falsification: big-endian is not merely
    /// worse, it is absurd. If someone ever "fixes" the byte order, this says
    /// what they would be claiming.
    #[test]
    fn big_endian_would_be_nonsense_on_the_real_capture() {
        let capture = Capture::deovr_quest();
        let event = capture.inbound()[0];
        let prefix = event.prefix_bytes().unwrap();
        let be = i64::from(i32::from_be_bytes(prefix));
        let payload = event.text.as_deref().unwrap().len() as i64;
        assert_ne!(be, payload);
        // Read big-endian this frame is either non-positive — dismissed as a
        // heartbeat, with its payload then read as a length prefix — or wildly
        // over the frame cap. Neither is survivable.
        assert!(be <= 0 || be > i64::from(crate::codec::MAX_FRAME_BYTES));
    }

    #[test]
    fn every_captured_frame_parses() {
        let capture = Capture::deovr_quest();
        let _ = packets(&capture.inbound());
    }

    /// Across the whole capture the key set never varied. Anything hoping for
    /// a title, a chapter or a script hint has to work from `path`.
    #[test]
    fn the_real_key_set_is_exactly_the_five_documented_fields() {
        let capture = Capture::deovr_quest();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for event in capture.inbound() {
            let value: serde_json::Value =
                serde_json::from_str(event.text.as_deref().unwrap()).unwrap();
            for key in value.as_object().unwrap().keys() {
                seen.insert(key.clone());
            }
        }
        let expected: BTreeSet<String> = [
            "currentTime",
            "duration",
            "path",
            "playbackSpeed",
            "playerState",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(seen, expected);
    }

    /// DeoVR reports a URL, not a path on the headset — and the filename
    /// survives as the last segment, which is what keeps name matching viable.
    #[test]
    fn media_identity_is_a_url_whose_last_segment_is_the_filename() {
        let capture = Capture::deovr_quest();
        let inbound = capture.inbound();
        let parsed = packets(&inbound);
        let identity = parsed[0].1.media_identity().unwrap();
        assert!(identity.starts_with("http://"), "got {identity}");

        let last = identity.rsplit('/').next().unwrap();
        assert!(
            last.ends_with(".mp4"),
            "last segment should be a filename: {last}"
        );
    }

    #[test]
    fn real_steady_state_packets_are_full_not_partial() {
        let capture = Capture::deovr_quest();
        for (_, packet) in packets(&capture.inbound()) {
            assert!(
                packet.path.is_some() && packet.duration.is_some(),
                "expected a full packet"
            );
        }
    }

    /// No heartbeat frames at all, in fourteen minutes. The fake used to
    /// interleave them every fourth tick.
    #[test]
    fn a_real_deovr_sent_no_heartbeat_frames() {
        let capture = Capture::deovr_quest();
        let heartbeats = capture
            .events
            .iter()
            .filter(|e| e.kind == "heartbeat")
            .count();
        assert_eq!(heartbeats, 0);
    }

    #[test]
    fn the_observed_cadence_is_about_one_second() {
        let capture = Capture::deovr_quest();
        let frames = capture.longest_connection();
        let mut gaps: Vec<u64> = frames
            .windows(2)
            .map(|w| w[1].at_ms.saturating_sub(w[0].at_ms))
            .filter(|g| *g < 5000)
            .collect();
        gaps.sort_unstable();
        let median = gaps[gaps.len() / 2];
        assert!(
            (950..=1100).contains(&median),
            "median cadence outside 1 Hz: {median} ms"
        );
    }

    /// The premise the deadman design rests on, asserted against the evidence
    /// for it. If a future capture stalls mid-connection, this is the test that
    /// should fail and the design that should be revisited.
    #[test]
    fn a_live_connection_never_stalled() {
        let capture = Capture::deovr_quest();
        let frames = capture.longest_connection();
        for pair in frames.windows(2) {
            let gap = pair[1].at_ms - pair[0].at_ms;
            assert!(
                gap < 2000,
                "a {gap} ms stall inside one connection falsifies \
                 'a live connection is continuous'"
            );
        }
    }

    /// Reconnects, and the backwards jump across one of them. This is why a
    /// consumer needs `epoch` rather than an inference from position.
    #[test]
    fn position_went_backwards_across_a_reconnect() {
        let capture = Capture::deovr_quest();
        let inbound = capture.inbound();
        let parsed = packets(&inbound);
        let biggest_drop = parsed
            .windows(2)
            .map(|w| w[1].1.current_time.unwrap() - w[0].1.current_time.unwrap())
            .fold(0.0f64, |acc, d| acc.min(d));
        assert!(
            biggest_drop < -100.0,
            "expected the recorded backwards jump, got {biggest_drop}"
        );
    }

    /// The contradiction, pinned so nobody has to rediscover it: a run of 140
    /// frames reporting "paused" while position tracked the wall clock.
    #[test]
    fn player_state_one_appears_both_paused_and_playing() {
        let capture = Capture::deovr_quest();
        let frames = packets(&capture.longest_connection());

        let mut static_ones = 0;
        let mut advancing_ones = 0;
        for pair in frames.windows(2) {
            let dt = (pair[1].0 - pair[0].0) as f64 / 1000.0;
            if !(0.5..3.0).contains(&dt) {
                continue;
            }
            let rate = (pair[1].1.current_time.unwrap() - pair[0].1.current_time.unwrap()) / dt;
            if pair[1].1.player_state == Some(1) {
                if rate.abs() < 0.1 {
                    static_ones += 1;
                } else if rate > 0.85 {
                    advancing_ones += 1;
                }
            }
        }
        assert!(static_ones > 0, "state 1 should sometimes mean paused");
        assert!(
            advancing_ones > 50,
            "state 1 was observed advancing for over two minutes; got {advancing_ones} samples"
        );
    }

    /// And the snapshot must notice it rather than reporting "paused" to a
    /// consumer that would act on it.
    #[test]
    fn the_snapshot_flags_a_paused_player_that_keeps_moving() {
        let capture = Capture::deovr_quest();
        let mut snapshot = PlayerSnapshot::new("quest".into());
        for (at_ms, packet) in packets(&capture.longest_connection()) {
            snapshot.apply(&packet, at_ms);
        }
        assert!(
            snapshot.state_is_suspect(),
            "a 'paused' player advancing at 1.0x must be flagged, not accepted"
        );
    }

    /// The ghost this capture caught in the wild: a frame published 3.9 s
    /// *after* the user pressed Disconnect, because the detached read task was
    /// still running. See `player.rs` — the read task is now abort-on-drop.
    #[test]
    fn the_capture_contains_the_ghost_frame_that_proved_the_leak() {
        let capture = Capture::deovr_quest();
        let by_request = capture
            .events
            .iter()
            .find(|e| e.text.as_deref().is_some_and(|t| t.contains("by request")))
            .expect("the capture ends with a deliberate disconnect");

        let after: Vec<_> = capture
            .events
            .iter()
            .filter(|e| e.at_ms > by_request.at_ms && e.is_inbound_json())
            .collect();

        assert_eq!(
            after.len(),
            1,
            "the recorded leak was one frame arriving after Disconnect"
        );
        let late_by = after[0].at_ms - by_request.at_ms;
        assert!(
            late_by > 3000,
            "the ghost frame arrived {late_by} ms after the user disconnected"
        );
    }
}
