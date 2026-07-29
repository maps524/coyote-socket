# Fixtures

## `deovr-quest-2026-07-28.wire.jsonl`

The bridge's wire tap, unmodified, from a session against **DeoVR on a Meta
Quest** over Wi-Fi. 450 events: 418 inbound JSON frames, 21 outbound commands,
9 link events, 2 read errors, spanning **837.9 seconds** across **three
connections**.

This is the primary evidence for every empirical claim in `../README.md` and
`../src/capture.rs`. It is committed raw rather than transformed, because a
recording that has been through our own parser is a recording of our own
parser.

### Why `prefixHex` is the important column

Each frame carries the four length bytes **as they arrived**, before decoding:

```json
{"prefixHex":"c6 00 00 00","len":198,"text":"{\"path\":\"http://…\",…}"}
```

`c6 00 00 00` read little-endian is 198, and the payload is 198 UTF-8 bytes.
Read big-endian the same bytes are 3,321,888,768. That holds for **418 of 418**
frames.

This is what makes the fixture non-circular. The previous fixture was built
from `log_debug!` of the already-decoded string, so replaying it re-framed the
payload with our own encoder — which could only ever demonstrate that our
decoder agrees with our encoder. Byte order is now **observed from a real
player**, not inferred from MultiFunPlayer using `BitConverter` on a
little-endian host.

### Privacy

The `path` field is left exactly as received, including the LAN address of a
media server (`192.168.0.4`, an RFC1918 address meaningful only inside that
house) and the media filename, repeated on all 418 frames.

Both are load-bearing: the URL shape is why we know DeoVR reports a URL rather
than a filesystem path, and the filename in the last segment is why name-based
script matching is still viable. **If this repository is published, that is a
decision to make deliberately rather than by omission.** Substituting a
placeholder filename would keep the framing and cadence evidence intact and
cost only the "the filename survives" observation, which could be restated in
prose.
