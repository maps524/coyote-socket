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

## `ums-browse-2026-07-29.soap.xml`

A verbatim `Browse` response from **Universal Media Server 15.7.0** (Linux),
`ObjectID` 134, `RequestedCount` 3. Committed whole, unparsed and untrimmed.

It is the evidence for `upnp.rs`'s claims about what a real ContentDirectory
returns, and it exists because a hand-written fixture was being cited for them.

### What it settles

- **Every video carries six `<res>`: one `video/mp4` and five thumbnails**, two
  PNG and three JPEG. That ratio is why `choose_res` filters resources by the
  item's `upnp:class` before ranking anything — without it a video whose own
  resource leaves `DLNA.ORG_OP` unstated can lose to a 160x160 PNG, and the
  picker hands a still image to a `<video>` element.
- **A container carries thumbnails too**, five of them, which is why the counts
  differ depending on whether you count per item or per document.
- **UMS offers no Matroska at all.** See below.

### The correction it forced

`upnp.rs` used to say that Universal Media Server lists `video/x-matroska`
ahead of `video/mp4` for the same item. It does not. That claim came from
`BROWSE_RESPONSE` in the module's own tests — a **hand-written** document,
written specifically to exercise the Matroska exclusion — and it travelled from
there into a doc comment and then into a pull-request description as evidence
about a real server.

The exclusion is still right for servers that do offer Matroska. The claim about
this one was not, and only fetching the real bytes caught it.

### The rule that follows

**A fixture is an assertion about the world, not an observation of it.** It
agrees with you because you wrote it to. The trap is that a fixture and a
capture are indistinguishable at the point of use — same directory, same
extension, same shape — so a test passing against a hand-written response feels
exactly like a test passing against a recorded one.

- Hand-written fixtures say **"hand-written"** in a comment, at the top.
  `BROWSE_RESPONSE` now does.
- Captures say what they came from, when, and which version.
- **Never cite a fixture as evidence about the world.** It is evidence about the
  code's behaviour on an input, which is a different sentence.
- **Keep captures whole.** A trimmed capture is a fixture again: what was
  trimmed was chosen, and the choice is the assertion. This file is mostly
  thumbnails, which is exactly the part that would have been cut as noise — and
  exactly the part that mattered.

## `proxy-probe.html`

Not a recording. A page that loads a proxied video, plays it, seeks, and reads a
frame back through a canvas, so that "seeked" means a decoded picture rather
than a `currentTime` that changed. `../README.md` has how to run it and the trap
that `--dump-dom` does not work for media.
