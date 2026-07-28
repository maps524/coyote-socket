//! The bridge's normalised view of the player, and the packet shapes on the
//! TCP side.
//!
//! `03-media-sources.md` lists the message set MFP normalises every source
//! onto (`MediaPathChanged`, `MediaPositionChanged`, `MediaPlayPause`,
//! `MediaDurationChanged`, `MediaSpeedChanged`). Rather than five event types,
//! this carries one snapshot with those five facts on it — the WebSocket
//! client wants current state, not a change log, and a reconnecting phone must
//! be told everything anyway.

use serde::{Deserialize, Serialize};

/// A packet received from the player.
///
/// Every field is optional. The player sends partial updates: DeoVR emits a
/// packet with only `currentTime` while scrubbing, and a full one on load.
/// Fields are merged into `PlayerSnapshot` rather than replacing it.
///
/// `path`, `playerState`, `duration`, `currentTime` and `playbackSpeed` are
/// confirmed from DeoVR's published docs and from MFP's two source files.
///
/// `resource` and `identifier` are **low confidence** — they turned up while
/// reading HereSphere's adapter and are treated as fallbacks for media
/// identity only. If a real HereSphere never sends them, nothing breaks.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerPacket {
    pub path: Option<String>,
    pub resource: Option<String>,
    pub identifier: Option<String>,
    /// 0 = playing, 1 = paused. Documented by DeoVR as `Play = 0, Pause = 1`.
    pub player_state: Option<i32>,
    pub duration: Option<f64>,
    pub current_time: Option<f64>,
    pub playback_speed: Option<f64>,
}

impl PlayerPacket {
    /// Best available media identity. DeoVR sends `path`; the HereSphere
    /// fallbacks are tried after it.
    pub fn media_identity(&self) -> Option<&str> {
        self.path
            .as_deref()
            .or(self.resource.as_deref())
            .or(self.identifier.as_deref())
            .filter(|s| !s.is_empty())
    }
}

/// A packet sent *to* the player, to drive it from the phone.
///
/// Out of scope for what the spike had to prove, but it costs almost nothing
/// once the write half exists, and `03-media-sources.md` calls out that
/// controlling the video from the phone is a large part of the "one device"
/// experience. Field names mirror MFP's outgoing `PlayerState`.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerCommand {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_time: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playback_speed: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub player_state: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
}

/// How the bridge's TCP link to the player is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkState {
    /// No connection attempt has succeeded yet, or the last one dropped.
    /// This is the normal state during development — the player is usually
    /// not running.
    Disconnected,
    Connecting,
    Connected,
}

/// Everything the bridge knows about playback, as one snapshot.
///
/// This is the payload the phone receives. It is deliberately explicit about
/// what is *unknown*: `position_s: None` means "the player has not told us",
/// which is not the same as position zero. A consumer that treats missing as
/// zero would jump the script to the start.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerSnapshot {
    /// Always `"player"`. Reserved so the WebSocket can carry other message
    /// types later (T-Code axis frames, script availability) without the
    /// client having to guess.
    #[serde(rename = "type")]
    pub kind: &'static str,

    pub link: LinkState,
    /// The `host:port` the bridge is dialling.
    pub endpoint: String,

    /// Media identity as the *player* reports it. Almost certainly a path on
    /// the headset's filesystem or a URL, and almost certainly not a path the
    /// phone can resolve. MFP has `MediaPathModifier`s for exactly this;
    /// mapping is left to the consumer for now.
    pub media: Option<String>,
    pub position_s: Option<f64>,
    pub duration_s: Option<f64>,
    /// Derived from `playerState`: `Some(true)` when playing, `Some(false)`
    /// when paused, `None` when the player has never said.
    pub playing: Option<bool>,
    pub speed: Option<f64>,

    /// Bridge-local wall clock (ms since epoch) at which the last packet
    /// arrived. Lets the phone age out a stale position rather than
    /// extrapolating from a frozen one.
    pub updated_at_ms: u64,
    /// Count of JSON packets received on this connection. Purely diagnostic,
    /// but it is the fastest way to tell "connected but silent" from
    /// "connected and streaming" when looking at a live page.
    pub packets: u64,
}

impl PlayerSnapshot {
    pub fn new(endpoint: String) -> Self {
        Self {
            kind: "player",
            link: LinkState::Disconnected,
            endpoint,
            media: None,
            position_s: None,
            duration_s: None,
            playing: None,
            speed: None,
            updated_at_ms: 0,
            packets: 0,
        }
    }

    /// Merge a packet in. Absent fields leave the previous value alone.
    pub fn apply(&mut self, packet: &PlayerPacket, now_ms: u64) {
        if let Some(media) = packet.media_identity() {
            if self.media.as_deref() != Some(media) {
                // A new file means any script the phone had loaded is wrong,
                // and position continuity is broken. Clear the derived facts
                // rather than letting the old duration linger.
                self.media = Some(media.to_string());
                self.duration_s = None;
                self.position_s = None;
            }
        }
        if let Some(state) = packet.player_state {
            self.playing = Some(state == 0);
        }
        if let Some(d) = packet.duration {
            self.duration_s = Some(d);
        }
        if let Some(t) = packet.current_time {
            self.position_s = Some(t);
        }
        if let Some(s) = packet.playback_speed {
            self.speed = Some(s);
        }
        self.updated_at_ms = now_ms;
        self.packets += 1;
    }

    /// Reset the playback facts on disconnect, keeping the endpoint. The
    /// phone must not keep acting on a position from a player that is gone.
    pub fn on_disconnect(&mut self, now_ms: u64) {
        self.link = LinkState::Disconnected;
        self.media = None;
        self.position_s = None;
        self.duration_s = None;
        self.playing = None;
        self.speed = None;
        self.updated_at_ms = now_ms;
        self.packets = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(json: &str) -> PlayerPacket {
        serde_json::from_str(json).expect("packet should parse")
    }

    #[test]
    fn parses_a_full_deovr_packet() {
        let p = packet(
            r#"{"path":"C:\\vr\\clip.mp4","duration":600.0,"currentTime":12.5,
                "playbackSpeed":1.0,"playerState":0}"#,
        );
        assert_eq!(p.media_identity(), Some("C:\\vr\\clip.mp4"));
        assert_eq!(p.current_time, Some(12.5));
        assert_eq!(p.player_state, Some(0));
    }

    #[test]
    fn tolerates_unknown_fields_and_partial_packets() {
        // A real player will send fields this spike has never seen. Refusing
        // to parse the packet would be the worst possible failure mode.
        let p = packet(r#"{"currentTime":3.25,"somethingNew":{"a":1}}"#);
        assert_eq!(p.current_time, Some(3.25));
        assert_eq!(p.path, None);
    }

    #[test]
    fn partial_updates_merge_rather_than_replace() {
        let mut snap = PlayerSnapshot::new("192.168.1.50:23554".into());
        snap.apply(
            &packet(r#"{"path":"a.mp4","duration":600.0,"currentTime":0.0,"playerState":0}"#),
            100,
        );
        // Scrub update carries position only.
        snap.apply(&packet(r#"{"currentTime":42.0}"#), 200);

        assert_eq!(snap.position_s, Some(42.0));
        assert_eq!(snap.duration_s, Some(600.0), "duration must survive");
        assert_eq!(snap.media.as_deref(), Some("a.mp4"));
        assert_eq!(snap.playing, Some(true));
        assert_eq!(snap.updated_at_ms, 200);
        assert_eq!(snap.packets, 2);
    }

    #[test]
    fn player_state_one_means_paused() {
        let mut snap = PlayerSnapshot::new("x".into());
        snap.apply(&packet(r#"{"playerState":1}"#), 1);
        assert_eq!(snap.playing, Some(false));
        snap.apply(&packet(r#"{"playerState":0}"#), 2);
        assert_eq!(snap.playing, Some(true));
    }

    #[test]
    fn changing_media_clears_stale_duration_and_position() {
        let mut snap = PlayerSnapshot::new("x".into());
        snap.apply(
            &packet(r#"{"path":"a.mp4","duration":600.0,"currentTime":590.0}"#),
            1,
        );
        snap.apply(&packet(r#"{"path":"b.mp4"}"#), 2);
        assert_eq!(snap.media.as_deref(), Some("b.mp4"));
        assert_eq!(snap.duration_s, None, "600s belonged to a.mp4");
        assert_eq!(snap.position_s, None);
    }

    #[test]
    fn repeating_the_same_path_does_not_clear_position() {
        // DeoVR re-sends the full packet periodically; treating every one as a
        // media change would wipe position 1 Hz.
        let mut snap = PlayerSnapshot::new("x".into());
        snap.apply(
            &packet(r#"{"path":"a.mp4","duration":600.0,"currentTime":10.0}"#),
            1,
        );
        snap.apply(&packet(r#"{"path":"a.mp4","currentTime":11.0}"#), 2);
        assert_eq!(snap.position_s, Some(11.0));
        assert_eq!(snap.duration_s, Some(600.0));
    }

    #[test]
    fn disconnect_clears_playback_but_keeps_endpoint() {
        let mut snap = PlayerSnapshot::new("192.168.1.50:23554".into());
        snap.apply(
            &packet(r#"{"path":"a.mp4","currentTime":10.0,"playerState":0}"#),
            1,
        );
        snap.on_disconnect(5);
        assert_eq!(snap.link, LinkState::Disconnected);
        assert_eq!(snap.position_s, None);
        assert_eq!(snap.playing, None);
        assert_eq!(snap.endpoint, "192.168.1.50:23554");
    }

    #[test]
    fn snapshot_serialises_with_camel_case_keys() {
        let snap = PlayerSnapshot::new("h:23554".into());
        let v: serde_json::Value = serde_json::to_value(&snap).unwrap();
        assert_eq!(v["type"], "player");
        assert_eq!(v["link"], "disconnected");
        assert!(v.get("positionS").is_some());
        assert!(v.get("updatedAtMs").is_some());
        // Unknown position must be null, never 0.
        assert!(v["positionS"].is_null());
    }

    #[test]
    fn command_omits_unset_fields() {
        let cmd = PlayerCommand {
            current_time: Some(30.0),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_string(&cmd).unwrap(),
            r#"{"currentTime":30.0}"#
        );
    }
}
