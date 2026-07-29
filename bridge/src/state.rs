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
///
/// Four states rather than three, because "we are not trying" and "we are
/// trying and failing" are different situations and a user needs to be told
/// which one they are in. The original spike collapsed both into
/// `Disconnected`, which made a bridge that had never been asked to connect
/// look identical to one that could not reach the headset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkState {
    /// Nobody has asked for a connection, or the user disconnected.
    Idle,
    /// An attempt is in flight — probing or handshaking.
    Connecting,
    Connected,
    /// The last attempt failed and we are backing off before the next.
    /// [`PlayerSnapshot::fault`] says why.
    Retrying,
}

/// What went wrong, in terms someone can act on.
///
/// Split by *cause* rather than by error code because the actions differ:
/// a refusal means "the player is not listening — switch remote control on",
/// a timeout means "wrong address, or the headset is asleep", and a framing
/// fault means "our reading of the protocol is wrong", which is the finding
/// this whole spike exists to surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FaultKind {
    /// The host answered and refused the port. Almost always: remote control
    /// is switched off in the player's settings.
    Refused,
    /// Nothing answered in time. Wrong IP, asleep headset, other network, or
    /// a firewall dropping the SYN.
    TimedOut,
    /// The OS says there is no route to that host at all.
    Unreachable,
    /// The endpoint could not be parsed or resolved.
    Address,
    /// We were connected and the player hung up.
    Closed,
    /// The bytes did not fit the protocol. **This is the interesting one** —
    /// it means our reading of the framing is wrong, not that the network is.
    Framing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkFault {
    pub kind: FaultKind,
    /// The underlying error, verbatim. Never paraphrased away — if our
    /// explanation is wrong, this is what lets someone see that.
    pub detail: String,
    /// What to try. `None` when we genuinely have no suggestion, which is
    /// better than inventing one.
    pub hint: Option<String>,
}

impl LinkFault {
    pub fn new(kind: FaultKind, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        let hint = default_hint(kind);
        Self { kind, detail, hint }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

fn default_hint(kind: FaultKind) -> Option<String> {
    Some(match kind {
        FaultKind::Refused => "The headset answered but nothing is listening on that port. \
             Neither DeoVR nor HereSphere opens 23554 until remote control is \
             switched on in the player's own settings, and you have to be inside \
             the video player. Turn it on and try again."
            .into(),
        FaultKind::TimedOut => "No answer at all. Check the address, check the headset is awake \
             and on the same network, and check nothing is filtering the port."
            .into(),
        FaultKind::Unreachable => {
            "No route to that address. The two devices are probably on different networks.".into()
        }
        FaultKind::Address => "That does not look like a reachable address. Use the headset's \
             IP, optionally with :23554."
            .into(),
        FaultKind::Closed => "The player closed the connection. If this happens about three \
             seconds after connecting, our keepalive is not reaching it."
            .into(),
        FaultKind::Framing => "The bytes on the wire did not match what we expected. This is a \
             finding, not a glitch — copy the wire log and send it back."
            .into(),
    })
}

/// Classify a connection error into something a person can act on.
pub fn classify_connect_error(e: &std::io::Error) -> LinkFault {
    use std::io::ErrorKind::*;
    let kind = match e.kind() {
        ConnectionRefused | ConnectionReset => FaultKind::Refused,
        TimedOut => FaultKind::TimedOut,
        // `HostUnreachable` / `NetworkUnreachable` are still unstable to name
        // on some targets, so fall back to the message.
        _ if e.to_string().to_lowercase().contains("unreachable") => FaultKind::Unreachable,
        InvalidInput | NotFound => FaultKind::Address,
        _ => FaultKind::TimedOut,
    };
    LinkFault::new(kind, e.to_string())
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
    ///
    /// **Advisory only. Never gate output on this.** Against a real DeoVR,
    /// `playerState` was observed to echo the last value a remote client set
    /// rather than to report what the player is doing — it stayed at `1`
    /// ("paused") through two minutes of playback at 1.0×. A pause performed
    /// inside the headset does not reach this field at all. See `capture.rs`.
    ///
    /// The reliable signal is whether [`Self::position_s`] is advancing, and
    /// that signal is inherently late: two consecutive packets are needed to
    /// establish that position stopped, and DeoVR's observed cadence is
    /// ~1010 ms, so a pause is undetectable for **1.0–2.0 s** — a floor, not
    /// an estimate. Anything driving hardware from script position has that
    /// window to account for. Position holds rather than climbs during it, so
    /// output does not run away; but a script's decline to zero will not have
    /// started either.
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

    /// Why the last attempt failed, when it did. Cleared on a successful
    /// connection so a stale explanation never sits under a working link.
    pub fault: Option<LinkFault>,
    /// Connection attempts since the user last asked to connect. Makes a
    /// silent retry loop visible instead of leaving the UI looking frozen.
    pub attempts: u32,

    /// Set when the player reports itself paused while its position advances
    /// in step with the wall clock.
    ///
    /// This is not a defensive nicety — it fired on the first real capture we
    /// ever took. DeoVR sent `playerState: 1` (documented as *paused*) through
    /// three seconds of position advancing at exactly 1.0×. Either the
    /// documented mapping is wrong or something stranger is happening, and
    /// three frames from one session are not enough to decide. Rather than
    /// silently flipping a mapping that DeoVR's own docs and MultiFunPlayer
    /// both corroborate, the contradiction is carried on the snapshot so the
    /// next session settles it. See `capture.rs`.
    pub state_suspect: bool,
}

impl PlayerSnapshot {
    pub fn new(endpoint: String) -> Self {
        Self {
            kind: "player",
            link: LinkState::Idle,
            endpoint,
            media: None,
            position_s: None,
            duration_s: None,
            playing: None,
            speed: None,
            updated_at_ms: 0,
            packets: 0,
            fault: None,
            attempts: 0,
            state_suspect: false,
        }
    }

    /// Whether the player's own play/pause flag disagrees with its position.
    pub fn state_is_suspect(&self) -> bool {
        self.state_suspect
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
            // Compare against the previous position before overwriting it: a
            // "paused" player whose position tracks the wall clock is telling
            // us something about the protocol, not about the video.
            if let Some(previous) = self.position_s {
                let elapsed = now_ms.saturating_sub(self.updated_at_ms) as f64 / 1000.0;
                let moved = t - previous;
                let tracks_wall_clock =
                    elapsed > 0.2 && moved > 0.2 && (moved - elapsed).abs() < elapsed * 0.5;
                if self.playing == Some(false) && tracks_wall_clock {
                    self.state_suspect = true;
                }
            }
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
    ///
    /// `link` is left for the caller to set: the supervisor knows whether this
    /// is a retry or a deliberate disconnect, and this function does not.
    pub fn on_disconnect(&mut self, now_ms: u64) {
        self.media = None;
        self.position_s = None;
        self.duration_s = None;
        self.playing = None;
        self.speed = None;
        self.updated_at_ms = now_ms;
        self.packets = 0;
        self.state_suspect = false;
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
        assert_eq!(snap.position_s, None);
        assert_eq!(snap.playing, None);
        assert_eq!(snap.endpoint, "192.168.1.50:23554");
    }

    #[test]
    fn snapshot_serialises_with_camel_case_keys() {
        let snap = PlayerSnapshot::new("h:23554".into());
        let v: serde_json::Value = serde_json::to_value(&snap).unwrap();
        assert_eq!(v["type"], "player");
        assert_eq!(v["link"], "idle");
        assert!(v.get("positionS").is_some());
        assert!(v.get("updatedAtMs").is_some());
        // Unknown position must be null, never 0.
        assert!(v["positionS"].is_null());
    }

    /// The distinction the Quest test depends on: a refusal and a timeout must
    /// not read the same, because they call for different actions.
    #[test]
    fn a_refusal_is_explained_as_remote_control_being_off() {
        let fault = classify_connect_error(&std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "connection refused",
        ));
        assert_eq!(fault.kind, FaultKind::Refused);
        let hint = fault.hint.unwrap();
        assert!(hint.contains("remote control"), "got: {hint}");
    }

    #[test]
    fn a_timeout_is_explained_as_an_addressing_problem() {
        let fault = classify_connect_error(&std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "timed out",
        ));
        assert_eq!(fault.kind, FaultKind::TimedOut);
        assert!(fault.hint.unwrap().contains("address"));
    }

    #[test]
    fn faults_keep_the_underlying_error_verbatim() {
        // If our explanation is wrong, the raw text is what reveals it.
        let fault = classify_connect_error(&std::io::Error::other("something we did not model"));
        assert_eq!(fault.detail, "something we did not model");
    }

    #[test]
    fn a_fault_serialises_with_a_kebab_case_kind() {
        let v = serde_json::to_value(LinkFault::new(FaultKind::TimedOut, "x")).unwrap();
        assert_eq!(v["kind"], "timed-out");
        assert_eq!(v["detail"], "x");
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
