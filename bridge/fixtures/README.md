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

### It records two bugs that no longer exist

This file is now the reference anyone reads `kind` semantics off, so be clear
that two of its features are **artefacts of the code that recorded it**, not
things today's bridge produces.

**The two `kind: "error"` events are misclassifications.** Both are
`os error 10054` — `WSAECONNRESET`, the headset going to sleep. At capture time
any non-EOF read error was classified as a framing failure, so an ordinary
disconnect raised the loudest alarm the app has, complete with a note saying
"this is the result the spike was looking for". It was not. Today reset, abort,
broken pipe and timeout are disconnects, and only a genuine decode failure is
`kind: "error"` — see `player::is_disconnect`.

**The last frame is a ghost.** `seq=449`, at t+837.9 s, arrives **3.9 seconds
after** the `disconnected … by request` event at `seq=448`. The read task had
been detached rather than aborted, so it kept reading the socket and publishing
state for a connection the user had already disconnected from. That single
frame is the evidence that closed the bug; the read task is now held in an
abort-on-drop guard, and `capture.rs` asserts the frame is still here so the
regression has a witness.

There is **exactly one** such frame in this capture, by the definition
"inbound frame arriving while no connection was live". The six frames at
`seq=261..266` look like a second run of them but are not: they follow the
`connected to` event at `seq=260` and belong to connection 2, which was
genuinely short-lived — it lasted five seconds before the headset reset it
again. Sequence numbers are monotonic across the file and positions never
overlap between connections, which is what a single reader looks like.

`Capture::longest_connection()` excludes the ghost and both short connections,
so replay never serves either artefact.

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
