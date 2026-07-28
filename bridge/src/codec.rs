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
//! - **Length prefix is 4 bytes, little-endian, signed** — high. The
//!   endianness is inferred rather than documented: `BitConverter` follows
//!   host byte order, and MFP is a Windows/x86 application, so little-endian.
//!   No player is going to be big-endian, but this is the one detail that came
//!   from inference rather than a spec sentence.
//! - **Zero-length frame is a heartbeat and must be tolerated inbound** — high.
//!   MFP's read loop skips non-positive lengths.
//! - **Keepalive cadence 1 Hz, player-side timeout 3 s** — high, documented.
//! - **UTF-8 payload** — high, documented.

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

/// Read exactly one frame. Cancel-safe only at frame boundaries: if this
/// future is dropped mid-frame the stream is left desynced, so call it from a
/// dedicated read task rather than inside a `select!` arm that can lose.
pub async fn read_frame<R>(reader: &mut R) -> io::Result<Frame>
where
    R: AsyncReadExt + Unpin,
{
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).await?;
    let length = i32::from_le_bytes(len_buf);

    if length <= 0 {
        return Ok(Frame::Heartbeat);
    }
    if length > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame length {length} exceeds {MAX_FRAME_BYTES} byte cap — stream desynced?"),
        ));
    }

    let mut payload = vec![0u8; length as usize];
    reader.read_exact(&mut payload).await?;

    String::from_utf8(payload)
        .map(Frame::Json)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
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
