//! Framing for the DeoVR / HereSphere remote-control protocol (TCP 23554).
//!
//! # Where these assumptions come from
//!
//! Two independent sources, which agree:
//!
//! 1. **DeoVR's official remote-control documentation** (<https://deovr.com/app/doc>):
//!    - "Each packet starts with 4-bytes integer value with length of json data
//!      represented in UTF8 format."
//!    - "Remote client also must send a packet (empty or with json) to DeoVR
//!      each one second for pinging purposes."
//!    - "If DeoVR won't receive any type of packet for more then 3 seconds it
//!      will close the connection."
//!
//! 2. **MultiFunPlayer's `DeoVRMediaSource.cs` and `HereSphereMediaSource.cs`**
//!    (MIT, © Yoooi). Both read the prefix with `BitConverter.ToInt32(buf, 0)`
//!    and write it with `BitConverter.GetBytes(len)`, and both send
//!    `new byte[4]` on a 1000 ms timer as keepalive. The two files use
//!    identical framing, which is why one adapter covers both players.
//!
//! # Confidence
//!
//! - **Length prefix is 4 bytes, little-endian, signed** — settled. `DeoVRMediaSource.cs`
//!   reads it with `BitConverter.ToInt32(lengthBuffer, 0)` and writes it with
//!   `BitConverter.GetBytes(messageBytes.Length)`, with no byte-swap on either
//!   path. `BitConverter` follows host order, and MFP is a WPF application, so
//!   it only ever runs where `BitConverter.IsLittleEndian` is true. A working
//!   third-party client therefore emits and expects little-endian, and
//!   [`prefix_is_little_endian`](tests) fails loudly if this is ever flipped.
//! - **Zero-length frame is a heartbeat and must be tolerated inbound** — settled.
//!   MFP's read loop treats non-positive lengths as a reset rather than data.
//! - **Keepalive cadence 1 Hz, player-side timeout 3 s** — high, documented.
//! - **UTF-8 payload** — high, documented.
//!
//! What remains genuinely open is not the framing rules but whether a *real*
//! DeoVR or HereSphere follows them, and what fields it actually populates.
//! [`diagnose_prefix`] and [`diagnose_payload`] exist so that if it does not,
//! the failure arrives as a sentence rather than as a hang.

use std::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A zero-length frame. Sent as keepalive, accepted as one.
pub const HEARTBEAT: [u8; 4] = [0, 0, 0, 0];

/// Ceiling on an inbound frame so a desynced or hostile peer cannot make us
/// allocate arbitrarily. Real packets are a few hundred bytes; 1 MiB is
/// several orders of magnitude of headroom.
pub const MAX_FRAME_BYTES: i32 = 1024 * 1024;

/// Interval at which we must send *something* or the player hangs up.
/// Documented player-side timeout is 3 s; 1 Hz matches both the DeoVR docs
/// and MFP.
pub const KEEPALIVE_INTERVAL_MS: u64 = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// A zero (or negative) length prefix with no payload.
    Heartbeat,
    /// A UTF-8 JSON payload. Not parsed here — framing only.
    Json(String),
}

/// A frame plus the bytes that framed it.
///
/// The prefix is kept so a caller can show what actually arrived rather than
/// only what we made of it. That is the difference between "the bridge
/// disconnected" and "the peer sent `00 00 01 2c`, which we read as 738 197 504".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFrame {
    /// The four length bytes in wire order.
    pub prefix: [u8; 4],
    /// The prefix decoded as we believe it should be: little-endian, signed.
    pub len: i32,
    pub frame: Frame,
}

/// Read exactly one frame. Cancel-safe only at frame boundaries: if this
/// future is dropped mid-frame the stream is left desynced, so call it from a
/// dedicated read task rather than inside a `select!` arm that can lose.
pub async fn read_frame<R>(reader: &mut R) -> io::Result<Frame>
where
    R: AsyncReadExt + Unpin,
{
    read_frame_raw(reader).await.map(|raw| raw.frame)
}

/// As [`read_frame`], but keeps the length bytes.
///
/// Same cancel-safety caveat: frame boundaries only.
pub async fn read_frame_raw<R>(reader: &mut R) -> io::Result<RawFrame>
where
    R: AsyncReadExt + Unpin,
{
    let mut prefix = [0u8; 4];
    reader.read_exact(&mut prefix).await?;
    let length = i32::from_le_bytes(prefix);

    if length <= 0 {
        return Ok(RawFrame {
            prefix,
            len: length,
            frame: Frame::Heartbeat,
        });
    }
    if length > MAX_FRAME_BYTES {
        let mut message = format!(
            "length prefix [{}] decodes little-endian to {length}, over the \
             {MAX_FRAME_BYTES}-byte cap",
            crate::wire::hex4(prefix)
        );
        if let Some(diagnosis) = diagnose_prefix(prefix) {
            message.push_str(" — ");
            message.push_str(&diagnosis);
        }
        return Err(io::Error::new(io::ErrorKind::InvalidData, message));
    }

    let mut payload = vec![0u8; length as usize];
    reader.read_exact(&mut payload).await?;

    match String::from_utf8(payload) {
        Ok(text) => Ok(RawFrame {
            prefix,
            len: length,
            frame: Frame::Json(text),
        }),
        Err(e) => {
            let bytes = e.as_bytes();
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "payload after prefix [{}] ({length} bytes) is not UTF-8: {e}. \
                     First bytes: {}",
                    crate::wire::hex4(prefix),
                    hex_dump(bytes, 24)
                ),
            ))
        }
    }
}

/// Explain a length prefix that we could not use.
///
/// The single most valuable thing this can say is "the other end disagrees
/// with us about byte order", because that failure otherwise presents as an
/// enormous allocation or a hang, and looks nothing like its cause.
///
/// Returns `None` when there is nothing useful to say.
pub fn diagnose_prefix(prefix: [u8; 4]) -> Option<String> {
    let le = i32::from_le_bytes(prefix);
    let be = i32::from_be_bytes(prefix);

    // Big-endian reads as something plausible and little-endian does not: that
    // is the signature of a byte-order mismatch, not of a desync.
    if (le <= 0 || le > MAX_FRAME_BYTES) && be > 0 && be <= MAX_FRAME_BYTES {
        return Some(format!(
            "read big-endian the same bytes are {be}, which is a plausible packet size. \
             That would mean the peer disagrees with us about byte order — which the \
             sources say should not happen (MultiFunPlayer uses BitConverter on \
             Windows, so little-endian), and would be the single most important \
             finding of this test. Copy this line."
        ));
    }

    // Printable ASCII in the prefix means we are not at a frame boundary at
    // all: we started reading somewhere inside a payload.
    if prefix.iter().all(|b| (0x20..0x7f).contains(b)) {
        return Some(format!(
            "all four bytes are printable ASCII ({:?}) — this looks like payload text, \
             not a length. The stream is misaligned, so a previous frame's length was \
             wrong.",
            String::from_utf8_lossy(&prefix)
        ));
    }

    None
}

/// Explain a payload that framed correctly but does not look like the JSON the
/// protocol promises.
///
/// A frame that reads cleanly but contains a fragment of the *previous* packet
/// is what a subtly wrong length looks like, and it is easy to mistake for a
/// player quirk.
pub fn diagnose_payload(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    if trimmed.is_empty() {
        return Some("frame carried a positive length but no printable content".into());
    }
    if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
        return Some(format!(
            "payload does not start with '{{' or '[' — it begins {:?}. A correctly framed \
             packet is a JSON object, so the length prefix is probably not being applied \
             where we think it is.",
            trimmed.chars().take(24).collect::<String>()
        ));
    }
    if serde_json::from_str::<serde_json::Value>(text).is_err() {
        return Some(
            "payload starts like JSON but does not parse — likely truncated or \
             over-read, which points at the length prefix rather than at the content"
                .into(),
        );
    }
    None
}

/// `48 65 6c 6c 6f …` — first `max` bytes, for putting raw evidence in an
/// error message.
pub fn hex_dump(bytes: &[u8], max: usize) -> String {
    let shown: Vec<String> = bytes.iter().take(max).map(|b| format!("{b:02x}")).collect();
    let mut out = shown.join(" ");
    if bytes.len() > max {
        out.push_str(" …");
    }
    out
}

/// Length-prefix `json` and write it.
pub async fn write_json<W>(writer: &mut W, json: &str) -> io::Result<()>
where
    W: AsyncWriteExt + Unpin,
{
    let bytes = json.as_bytes();
    let len = i32::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "payload too large"))?;

    // One write call: the prefix and payload must not be interleaved with a
    // concurrent keepalive. Callers additionally hold the write half
    // exclusively, but building a single buffer makes that cheap to reason
    // about.
    let mut framed = Vec::with_capacity(4 + bytes.len());
    framed.extend_from_slice(&len.to_le_bytes());
    framed.extend_from_slice(bytes);
    writer.write_all(&framed).await
}

/// Write a zero-length keepalive frame.
pub async fn write_heartbeat<W>(writer: &mut W) -> io::Result<()>
where
    W: AsyncWriteExt + Unpin,
{
    writer.write_all(&HEARTBEAT).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reads_a_json_frame() {
        let mut wire: &[u8] = &[5, 0, 0, 0, b'h', b'e', b'l', b'l', b'o'];
        assert_eq!(
            read_frame(&mut wire).await.unwrap(),
            Frame::Json("hello".into())
        );
    }

    #[tokio::test]
    async fn zero_length_is_a_heartbeat_not_an_empty_string() {
        let mut wire: &[u8] = &HEARTBEAT;
        assert_eq!(read_frame(&mut wire).await.unwrap(), Frame::Heartbeat);
    }

    #[tokio::test]
    async fn negative_length_is_tolerated_as_a_heartbeat() {
        // The prefix is signed (BitConverter.ToInt32). MFP skips any
        // non-positive length rather than treating it as an error.
        let mut wire: &[u8] = &(-1i32).to_le_bytes();
        assert_eq!(read_frame(&mut wire).await.unwrap(), Frame::Heartbeat);
    }

    #[tokio::test]
    async fn prefix_is_little_endian() {
        // 256 bytes little-endian is [0,1,0,0]. Big-endian would read this as
        // 0x00010000 and try to allocate 64 KiB, so this test fails loudly if
        // the byte order is ever flipped.
        let mut wire = Vec::new();
        wire.extend_from_slice(&[0, 1, 0, 0]);
        wire.extend(std::iter::repeat_n(b'x', 256));
        let mut slice: &[u8] = &wire;
        match read_frame(&mut slice).await.unwrap() {
            Frame::Json(s) => assert_eq!(s.len(), 256),
            other => panic!("expected json frame, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn oversized_length_is_rejected_rather_than_allocated() {
        let mut wire: &[u8] = &(MAX_FRAME_BYTES + 1).to_le_bytes();
        let err = read_frame(&mut wire).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn raw_read_keeps_the_prefix_bytes() {
        let mut wire: &[u8] = &[2, 0, 0, 0, b'{', b'}'];
        let raw = read_frame_raw(&mut wire).await.unwrap();
        assert_eq!(raw.prefix, [2, 0, 0, 0]);
        assert_eq!(raw.len, 2);
        assert_eq!(raw.frame, Frame::Json("{}".into()));
    }

    /// The failure this whole diagnostic exists for: a peer that framed the
    /// packet big-endian. Little-endian reads it as ~700 million, which is
    /// meaningless; big-endian reads 300, which is exactly a packet.
    #[test]
    fn a_big_endian_peer_is_named_as_such() {
        let note = diagnose_prefix(300i32.to_be_bytes()).expect("should diagnose");
        assert!(note.contains("300"), "should quote the plausible size: {note}");
        assert!(note.contains("byte order"), "should name the cause: {note}");
    }

    #[test]
    fn a_prefix_of_text_is_reported_as_misalignment() {
        let note = diagnose_prefix(*b"path").expect("should diagnose");
        assert!(note.contains("misaligned"), "got: {note}");
    }

    #[test]
    fn an_ordinary_prefix_has_nothing_to_diagnose() {
        assert_eq!(diagnose_prefix(300i32.to_le_bytes()), None);
    }

    #[tokio::test]
    async fn an_oversized_prefix_carries_its_diagnosis_into_the_error() {
        // 0x7f000000 little-endian is absurd; big-endian it is 127, a
        // plausible packet.
        let mut wire: &[u8] = &127i32.to_be_bytes();
        let err = read_frame(&mut wire).await.unwrap_err();
        let text = err.to_string();
        assert!(text.contains("00 00 00 7f"), "raw bytes missing: {text}");
        assert!(text.contains("byte order"), "diagnosis missing: {text}");
    }

    #[test]
    fn payload_diagnosis_distinguishes_junk_from_json() {
        assert_eq!(diagnose_payload(r#"{"currentTime":1}"#), None);
        assert!(diagnose_payload("ath\":\"a.mp4\"}").is_some());
        assert!(diagnose_payload(r#"{"currentTime":"#).is_some());
        assert!(diagnose_payload("   ").is_some());
    }

    #[tokio::test]
    async fn a_non_utf8_payload_reports_the_bytes_it_saw() {
        let mut wire: &[u8] = &[3, 0, 0, 0, 0xff, 0xfe, 0xfd];
        let err = read_frame(&mut wire).await.unwrap_err();
        assert!(err.to_string().contains("ff fe fd"), "got: {err}");
    }

    #[tokio::test]
    async fn truncated_payload_is_an_error_not_a_short_read() {
        let mut wire: &[u8] = &[10, 0, 0, 0, b'a', b'b'];
        let err = read_frame(&mut wire).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[tokio::test]
    async fn round_trips_through_write_then_read() {
        let json = r#"{"path":"C:\\vr\\clip.mp4","currentTime":12.5,"playerState":0}"#;
        let mut buf = Vec::new();
        write_json(&mut buf, json).await.unwrap();
        assert_eq!(&buf[..4], &(json.len() as i32).to_le_bytes());

        let mut slice: &[u8] = &buf;
        assert_eq!(
            read_frame(&mut slice).await.unwrap(),
            Frame::Json(json.into())
        );
    }

    #[tokio::test]
    async fn multibyte_utf8_length_is_in_bytes_not_chars() {
        // A path with non-ASCII characters is the obvious way to get this
        // wrong: the prefix counts UTF-8 bytes, not `char`s.
        let json = r#"{"path":"日本語.mp4"}"#;
        let mut buf = Vec::new();
        write_json(&mut buf, json).await.unwrap();
        let prefix = i32::from_le_bytes(buf[..4].try_into().unwrap());
        assert_eq!(prefix as usize, json.len());
        assert_ne!(prefix as usize, json.chars().count());

        let mut slice: &[u8] = &buf;
        assert_eq!(
            read_frame(&mut slice).await.unwrap(),
            Frame::Json(json.into())
        );
    }

    #[tokio::test]
    async fn reads_consecutive_frames_including_interleaved_heartbeats() {
        let mut wire = Vec::new();
        write_json(&mut wire, "{}").await.unwrap();
        wire.extend_from_slice(&HEARTBEAT);
        write_json(&mut wire, r#"{"a":1}"#).await.unwrap();

        let mut slice: &[u8] = &wire;
        assert_eq!(
            read_frame(&mut slice).await.unwrap(),
            Frame::Json("{}".into())
        );
        assert_eq!(read_frame(&mut slice).await.unwrap(), Frame::Heartbeat);
        assert_eq!(
            read_frame(&mut slice).await.unwrap(),
            Frame::Json(r#"{"a":1}"#.into())
        );
    }
}
