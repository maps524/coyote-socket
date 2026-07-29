//! Recorded traffic from a real player, and what it told us.
//!
//! # Why this file exists
//!
//! Until 2026-07-28 every test in this repo was circular. The client was
//! written from DeoVR's published documentation and MultiFunPlayer's adapters;
//! `fake_player` was written from the same two documents by the same reading;
//! and the client was then tested against the fake. A shared misreading would
//! have passed every one of those tests. The README said so, and it was right
//! to.
//!
//! On 2026-07-28 the bridge connected to DeoVR on a real Quest and captured
//! three frames of steady-state playback. That is a small sample, but it is
//! *outside* our own assumptions, which is the property that matters. This
//! module carries that capture so it can be replayed instead of imagined.
//!
//! # What the capture settles, and what it does not
//!
//! Settled:
//!
//! - The framing works against a real DeoVR. Length-prefixed UTF-8 JSON,
//!   little-endian, is what a real player speaks. Every frame parsed.
//! - The five documented fields are all present and all populated.
//! - Our 1 Hz keepalive keeps a real player from hanging up: the connection
//!   lived across multiple seconds without being dropped.
//!
//! Not settled, and not even touched:
//!
//! - **HereSphere.** Never connected to. The claim that one adapter covers
//!   both players is still an inference from MFP's two source files being
//!   identical.
//! - **Seeking, media changes, teardown.** The capture is two seconds of
//!   uninterrupted playback.
//! - **`resource` / `identifier`.** Absent here, as expected — they were
//!   always HereSphere fallbacks.
//! - **Anything about a second player version or a second platform.**
//!
//! # Divergences from what `fake_player` was pretending to be
//!
//! Each of these is a place the fake was wrong, and therefore a place the
//! tests built on it proved less than they appeared to.
//!
//! | Behaviour | `fake_player` before | Real DeoVR |
//! |---|---|---|
//! | Packet shape | full packet every 8th tick, `currentTime`-only between | **full packet every time** |
//! | Cadence | 500 ms | **~1010 ms** |
//! | Heartbeat frames | interleaved every 4th tick | **none observed** |
//! | `path` | `C:\VR\fake-clip.mp4` | **an `http://` URL to a media server** |
//!
//! The first is the most consequential. `PlayerSnapshot::apply` merges rather
//! than replaces precisely so a `currentTime`-only update cannot wipe the
//! duration — and the real player never sends one. That logic is still correct
//! and still worth keeping (HereSphere may differ, and DeoVR may send partials
//! while scrubbing, which this capture cannot show), but it was never
//! exercised by anything real.
//!
//! # `playerState` latches, and that is the finding
//!
//! A second, longer session — 240 inbound frames over four minutes, with the
//! user playing, pausing, and seeking from the bridge and from inside the
//! headset — settles what three frames could not. Correlating `playerState`
//! against whether `currentTime` advanced at 1.0×:
//!
//! ```text
//!   t+  0 …  58 s   state 0    advancing 1.00×      playing
//!   t+ 63 …  67 s   state 1    static    0.01×      paused
//!   t+ 78 …  90 s   state 1    static    0.00×      paused
//!   t+ 90 … 210 s   state 1    advancing 1.00×      PLAYING, still reporting 1
//! ```
//!
//! **The documented polarity is correct.** `0` is playing and `1` is paused;
//! every pause in the capture coincides with `1`, and every stretch of
//! `0` advances at exactly 1.0×. Nothing here justifies changing the mapping,
//! and nothing has been changed.
//!
//! **But the field is not a live status.** The last two rows are the same
//! value with opposite realities. The last thing the bridge sent before
//! t+90 s was `{"playerState":1}`, and DeoVR reported `1` for the following
//! two minutes — through playback that plainly resumed. The behaviour is
//! consistent with `playerState` **echoing the last value a remote client set
//! rather than reporting what the player is doing**, so a pause or a play
//! performed inside the headset never reaches the field.
//!
//! The three-frame capture in this fixture is an instance of exactly that: a
//! session where the value had latched to `1` and playback was running.
//!
//! Two consequences, both already applied:
//!
//! 1. **Do not trust `playerState` alone.** Whether position is advancing is
//!    the more reliable signal, and it is the one a script sampler should use.
//!    [`PlayerSnapshot::state_is_suspect`] raises exactly this contradiction.
//! 2. **Command and status are separate mappings.** Driving the player with
//!    `playerState` works — the user's play, pause, forward and back all did
//!    the right thing on a real Quest. That says nothing about the status
//!    field, and the two must be reasoned about independently.
//!
//! # `path` has at least two shapes
//!
//! The captured `path` is an HTTP URL because the media was being streamed
//! from a DLNA server (Universal Media Server). A file on the headset would
//! presumably report a filesystem path, but **that form has not been
//! observed** and should not be assumed.
//!
//! For anything matching scripts by name, the useful detail is that the URL is
//! **not opaque** — the final segment is the real filename:
//!
//! ```text
//!   http://192.168.0.4:5001/ums/media/06b1f0ee-…-3defa580433a/253/Cock-Hero-Island-5-Episode-I.mp4
//!                                                                 ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
//! ```
//!
//! So name-based matching remains viable for streamed sources with this
//! server, provided the consumer takes the last segment, percent-decodes it,
//! and drops any query string. Whether every DLNA server does the same is
//! unknown; some expose opaque content ids, and for those, name matching
//! cannot work at all.
//!
//! DeoVR sends no separate title field. Across all 240 captured frames the key
//! set was exactly `path`, `duration`, `currentTime`, `playbackSpeed`,
//! `playerState` — there is no other field carrying a display name.

use serde::Deserialize;

/// The capture, embedded so tests need no fixture path and no I/O.
const DEOVR_QUEST: &str = include_str!("../fixtures/deovr-quest-2026-07-28.json");

#[derive(Debug, Clone, Deserialize)]
pub struct Capture {
    pub source: String,
    pub frames: Vec<CapturedFrame>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturedFrame {
    /// Bridge-local wall clock when the frame arrived.
    pub at_ms: u64,
    /// The payload exactly as received, before any parsing.
    pub json: String,
}

impl Capture {
    /// The DeoVR-on-a-Quest capture.
    pub fn deovr_quest() -> Self {
        serde_json::from_str(DEOVR_QUEST).expect("the embedded capture is valid JSON")
    }

    /// Gaps between consecutive frames, in milliseconds. What the real
    /// player's cadence actually was.
    pub fn gaps_ms(&self) -> Vec<u64> {
        self.frames
            .windows(2)
            .map(|pair| pair[1].at_ms.saturating_sub(pair[0].at_ms))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{PlayerPacket, PlayerSnapshot};

    /// The whole point: real bytes, through the real parser.
    #[test]
    fn every_captured_frame_parses() {
        let capture = Capture::deovr_quest();
        assert!(!capture.frames.is_empty());
        for frame in &capture.frames {
            serde_json::from_str::<PlayerPacket>(&frame.json)
                .unwrap_or_else(|e| panic!("real player frame failed to parse: {e}\n{}", frame.json));
        }
    }

    #[test]
    fn a_real_player_sends_every_documented_field() {
        let capture = Capture::deovr_quest();
        let packet: PlayerPacket = serde_json::from_str(&capture.frames[0].json).unwrap();
        assert!(packet.path.is_some());
        assert!(packet.duration.is_some());
        assert!(packet.current_time.is_some());
        assert!(packet.playback_speed.is_some());
        assert!(packet.player_state.is_some());
        // The HereSphere fallbacks were never DeoVR's to send.
        assert!(packet.resource.is_none());
        assert!(packet.identifier.is_none());
    }

    /// DeoVR reports a URL, not a path on the headset. Anything that later
    /// wants to find the matching funscript has to cope with that, and a fake
    /// that only ever said `C:\VR\clip.mp4` would never have shown it.
    #[test]
    fn media_identity_is_a_url() {
        let capture = Capture::deovr_quest();
        let packet: PlayerPacket = serde_json::from_str(&capture.frames[0].json).unwrap();
        let identity = packet.media_identity().unwrap();
        assert!(identity.starts_with("http://"), "got {identity}");
    }

    /// Every frame is a full packet. The fake used to send partials between
    /// full ones; the real player does not, at least not in steady state.
    #[test]
    fn real_steady_state_packets_are_full_not_partial() {
        for frame in &Capture::deovr_quest().frames {
            let packet: PlayerPacket = serde_json::from_str(&frame.json).unwrap();
            assert!(
                packet.path.is_some() && packet.duration.is_some(),
                "expected a full packet, got {}",
                frame.json
            );
        }
    }

    #[test]
    fn the_observed_cadence_is_about_one_second() {
        for gap in Capture::deovr_quest().gaps_ms() {
            assert!(
                (950..=1100).contains(&gap),
                "cadence outside the documented 1 Hz: {gap} ms"
            );
        }
    }

    /// The contradiction, pinned as a test so nobody has to rediscover it.
    ///
    /// If a future capture shows `playerState` behaving as documented, this
    /// test is the thing that should be revisited — not quietly deleted.
    #[test]
    fn player_state_contradicts_the_advancing_position() {
        let capture = Capture::deovr_quest();
        let mut snapshot = PlayerSnapshot::new("quest".into());
        for frame in &capture.frames {
            let packet: PlayerPacket = serde_json::from_str(&frame.json).unwrap();
            snapshot.apply(&packet, frame.at_ms);
        }

        // Our documented mapping says this is paused.
        assert_eq!(snapshot.playing, Some(false));

        let first: PlayerPacket = serde_json::from_str(&capture.frames[0].json).unwrap();
        let last: PlayerPacket =
            serde_json::from_str(&capture.frames[capture.frames.len() - 1].json).unwrap();
        let moved = last.current_time.unwrap() - first.current_time.unwrap();
        let elapsed = (capture.frames[capture.frames.len() - 1].at_ms - capture.frames[0].at_ms)
            as f64
            / 1000.0;

        assert!(
            (moved - elapsed).abs() < 0.05,
            "position tracked wall clock at 1.0x ({moved:.3}s over {elapsed:.3}s) — \
             so the media was playing while playerState said 1"
        );
        assert!(
            snapshot.state_is_suspect(),
            "the snapshot must notice that a 'paused' player is advancing"
        );
    }
}
