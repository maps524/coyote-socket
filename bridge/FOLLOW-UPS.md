# Bridge follow-ups

Sections **0**, **0a** and **0b** are not tasks. They are three defect signatures
this work produced repeatedly, written as recognition rules because each has
already caught its next instance. Read them before adding a field that records
whether something worked, a check that guards one, or an error message about a
component you do not own.

Sections **1** onward are work identified and deliberately not done.

---

## 0. The singleton assumption

Not a task. A stated assumption with its consequences, written down because it
has already produced four defects that looked unrelated to each other, and the
fifth will look unrelated to those.

**The assumption:** that there is exactly one of everything — one bridge on the
network, one instance of the process, one paired device, one writer of the
config directory.

Every one of those is true in the finished product and false during
development, which is the worst possible combination: the bugs are invisible to
the people writing the code and appear only to the people running it. Two
bridges at once is not an edge case while building a bridge — it is Tuesday.

### The four found so far

| Assumed singleton | What broke |
|---|---|
| One process holds the HTTP port | Bind failure was stored as `Option<String>`, where `None` meant both "fine" and "not asked yet". The window rendered a QR for a port it never got. |
| One responder to `coyote.local` | Both instances register mDNS, so instance B's install page could certify instance A's listener, report "Trusted", and send the phone to A carrying B's token. |
| One writer of the CA directory | Two instances racing an empty directory could load a key and a certificate that did not match, with nothing verifying they belonged together. |
| One paired device | Auto-rotating the token after a phone paired would silently un-pair every other device, because there is one shared token and no per-device identity. |

The first two are worth reading together, because **neither review predicted
the combination**. A trust check that blames the certificate for a listener
that never bound, plus a QR that scans perfectly and leads nowhere, compose
into an hour spent debugging TLS for a port collision. Each defect was found
separately. The interaction was found by accident.

### The shape, so the next one is recognisable

It appears as **a field that promises a capability nobody has confirmed**, and
it is almost always an `Option` doing two jobs: `None` meaning "no problem" and
"no answer yet" at the same time. The tell is that the optimistic reading is
the default, so the failure is silent and the UI is confident.

The fix is the same each time: make "not yet known" a state of its own, and set
the success state only after the runtime has confirmed it. Three values, not
two.

### The fifth, predicted and then found

Predicting it was cheap; checking took a minute and turned it from a guess into
a defect with a safety consequence, so it was fixed rather than filed.

**The settings file had one writer.** `Settings::save` was a whole-file
overwrite with no locking. Two instances each hold their own copy in memory:
instance A revokes a token it believes has leaked, and instance B — which read
the old token at startup — later saves for an unrelated reason and writes the
revoked token back. **The credential returns from the dead and nothing tells
anyone.** That is not untidiness; it is the revoke button silently not working,
and revocation a race can undo is worse than no revoke button, on the same
reasoning that retired the QR advertising a revoked token.

Fixed with an exclusive advisory lock (`File::lock`, released by the OS even on
a crash — a hand-rolled lockfile would trade this race for a worse one), a
read-modify-write merge where only the instance that minted or rotated a token
may write it, and a temp-file-plus-rename so a torn write cannot leave a file
that parses as defaults and mints a fresh token. The resurrection test was
confirmed to fail against the old implementation before the fix went in.

### Still open

- **The relay accepts many clients but the command channel is one queue.** Two
  phones both issuing `seek` will fight, and neither will be told. Harmless
  today because there is one phone; not harmless once pairing works.
- **Two instances mean two tray icons**, identical and unlabelled, with no way
  to tell which one holds the port.

### A related hazard: tests that reach off the machine

Same family, different axis — code that assumes it is the only thing on the
network, running somewhere it is not.

`bridge-tls` found a unit test multicasting `coyote.local -> 127.0.0.1` onto
the real LAN on every `cargo test`, able to break the user's phone from a
process that had already exited. Prompted by that, this crate's tests were
audited: **nothing here touches a real network.** Every listener binds
`127.0.0.1:0` and every dialled address is loopback. The LAN-looking addresses
in `probe`, `state` and `settings` tests are handled by string functions and
never connected to, and `http::local_ip` — which opens a UDP socket — is
called only from the two binaries' startup, never from a test.

The invariant is recorded on `probe`'s test module, since [`probe`] is the one
function that reaches outward on its own initiative: given a silent port it
scans sibling ports on the same host, so a single LAN address in a test would
port-scan a real machine on every run.

Ephemeral ports (`:0`) matter for the second reason: a fixed port in a test
collides with a bridge that is already running, which is the singleton
assumption again.

### The inverse, which is a real constraint and not an assumption

Worth keeping straight: **the player genuinely does accept one client at a
time.** That is not our assumption to relax — it is why `probe` must not run
while connected, and why a "test this address" button that opens a second
connection can sever the first. When auditing the list above, do not
"fix" that one.

---

## 0a. The inert guard

A variant of §0, written down separately because **§0's recognition rule does
not catch it**. Same tell — the optimistic reading is the default and the
failure is silent — but it hides somewhere §0 does not look. (§0b is a third
signature and independent of both; the three are siblings, not a sequence.)

**The shape:** a check whose success path is reached without the check
happening. The mechanism exists, is visible in review, and is inert.

That is what makes it expensive. Reading the code confirms the guard is
*present* and tells you nothing about whether it *runs*. A reviewer sees a test
covering the case, a warning covering the condition, an assertion covering the
regression — and every one of those observations is true and useless.

### The five, and how they were found

The first three were introduced **in the commits fixing §0 instances**, by
someone who had read this section that morning and was consciously applying it.
None was caught by writing it; all three were caught by a reviewer running the
code.

| The guard | When it was meant to run | When it actually ran |
|---|---|---|
| A test asserting every HTTP status carries its reason phrase | for every status a call site passes | for every status in a second hand-typed list inside the test |
| The regression test for symlink handling — this module's headline bug | every run | only where the machine can create symlinks; otherwise it printed a skip and reported `ok` |
| A warning that a file was excluded for exceeding the size cap | whenever a file was excluded | whenever the *script listing* changed, so a library whose only file was oversized said nothing |
| **`ScanState::Failed`** — the whole three-state mechanism from §0 | whenever the library could not be read | whenever the directory's *mtime moved or it would not `stat`*, so a directory that stats fine but refuses `read_dir` never triggered a scan, produced no verdict, and went on reporting `scan: "ok"` with a frozen `checkedAtMs` |
| A test asserting an oversized file is reported when it is the only entry | on the fix — the log in `poll` | on the precondition — `scan`'s return value, which is unchanged by reverting the fix |

Note how each one *nearly* works, and works in exactly the conditions you would
test it in. The status test passes for every status that exists today. The
symlink test runs on the developer's machine. The oversize warning fires the
moment there is a second, normal file in the directory. **The failing case is
the one nobody has yet**, which is the same reason §0's defects are invisible to
whoever writes them.

The fourth is the significant one, for three reasons.

**It is production code, not a test.** The first three were guards over the
code. This one *is* the code — the §0 three-state fix itself, complete and
correct and unreachable, because the gate deciding whether to run it asked a
different question than the check did. A reviewer had already confirmed the
state machine worked; it did, whenever anything invoked it.

**It was found by someone else applying the rule below**, on the diff that
introduced the rule, in one experiment: deny the list-directory right with
`icacls` while leaving attribute reads intact, and watch `checkedAtMs` stand
still for the full sixty seconds. That is better evidence than the first three,
which were found by their author noticing his own mistake.

**The obvious one-line fix was itself an inert guard**, and this is the part
worth remembering. Replacing the stat proxy with `state != ScanState::Ok` looks
exactly right — it retries until a scan succeeds and covers `Pending`. But the
transition *into* failure happens while `state` is still `Ok`, so the first
refusal is still missed, and the gate still never fires. It was written, the
test still failed, and only then did the actual shape become clear: **the gate
has to perform the same operation the check depends on.** So it now opens the
directory and takes one entry — O(1), every tick — rather than consulting a
`stat` or a flag that stands in for it. A proxy for the answer had been replaced
with a different proxy for the answer.

The fifth is small but is the best illustration of why the recognition rule has
to be asked deliberately: the assertion `scanned.oversize.len() == 1` looks
entirely reasonable, is true, and tests the wrong side of the fix. Both tests
now exist — one named as the precondition, one exercising `poll` through the log
tap — and the second was confirmed to fail against the reverted fix before it
was kept.

### The recognition rule

Deliberately not §0's. §0 asks *what does this value mean when nobody set it?*
— a question about state. This one is about control flow:

> **Under what condition does this check actually execute, and is that condition
> the same as the condition it is supposed to guard?**

Ask it out loud, of the guard rather than of the code being guarded. All five
above answer "no", and each answer is a sentence long once the question is
asked:

- the enumeration executes over a hand-written list, not over the call sites;
- the skip executes on a machine setting, not on the code under test;
- the warning executes when an unrelated list changed, not when a file was
  excluded;
- the scan executes when the mtime moved, not when the directory became
  unreadable;
- the assertion executes against `scan`'s return value, not against the `poll`
  branch that was changed.

None of those is hard to see. All five are invisible until someone asks.

**A test for a guard is itself a guard**, so ask it twice. The cheap way to
answer the second one is mechanical and worth doing every time: **revert the
fix, run the test, and require it to fail.** Both tests added for the fourth and
fifth instances were confirmed to fail against the reverted code before being
kept — and that is precisely how the one-line gate fix was caught being inert,
because the test kept failing after it went in.

### The fix, in preference order

1. **Make the failure unrepresentable**, so no guard is needed. The status case
   ended here: `respond` takes a `Status` carrying its own reason phrase, so a
   status without one is a compile error and the test that enumerated them is
   gone rather than fixed. A guard that cannot be inert is better than one that
   is currently running.
2. **Make the guard's trigger the same as the thing it guards.** The oversize
   warning now fires on the oversize set changing, not on the listing changing.
   The poll gate now *opens the directory* rather than consulting a `stat` or a
   state flag that stands in for opening it. Beware of satisfying this by
   swapping one proxy for another — that was the failed first attempt at the
   fourth instance, and it read as obviously correct.
3. **Fail rather than skip.** A skip that reports `ok` is an inert guard by
   construction. If a skip is genuinely unavoidable it has to be visible in
   default output — `eprintln!` is swallowed by libtest without `--nocapture`,
   so it is not.

Option 1 has a second benefit worth stating, because it is not obvious and it
generalises past this defect: **in a file several branches are editing, a type
error is a conflict that fails to build rather than one that resolves quietly.**
Four branches were touching `http.rs` when the status conversion landed. A
merge that drops a call site's status now stops the build; a merge that drops a
line from a hand-written test list does not.

### Where this sits relative to the review question

The house review question — *"what did this leave alone that now means
something different?"*, from **"The confident optional" in the sibling repo's
`CLAUDE.md`** (`coyote-socket-web`, not this one) — catches the change that
quietly reinterprets existing code. This is its sibling:

> **What did this add that never runs?**

One is about untouched code whose meaning moved; the other is about added code
that has no path to it. **Both are invisible in a diff**, which is why they
belong together: the diff shows the guard arriving and shows the surrounding
lines unchanged, and neither fact is the one that matters. Ask both of every
change that adds a check.

---

## 0b. Misattributed failure

A second signature, distinct from section 0 and worth separating because the
recognition rule is different. Section 0 is about **a field lying about
state**. This one is about **the diagnostic pointing at the wrong subsystem**.

> **When a failure is reported by a component that did not cause it, the fix
> belongs where the cause is and the *message* belongs where the user is
> looking.**

Both halves are needed. Fixing only the cause leaves the next occurrence just
as confusing; fixing only the message papers over a real defect.

### Five instances, three of them shipped

| What failed | What got blamed |
|---|---|
| The HTTPS listener never bound | The certificate. The trust-check page rendered in full and said "you missed the trust step." |
| The HTTP listener never bound | The phone, the Wi-Fi, then the certificate. The QR scanned perfectly and led nowhere. |
| A headset went to sleep (`WSAECONNRESET`) | **Our reading of the protocol.** Any non-EOF read error raised the framing alarm — the loudest signal in the app — for an ordinary disconnect. |
| An untrusted certificate on a `wss:` subresource | The network. It closes as 1006 with no interstitial and nothing the page can inspect, so it presents as "bridge unreachable". |
| `coyote.local` resolving to the WSL adapter | The certificate, again — for a name that never resolved to this machine at all. Avoided only because we wrote our own mDNS responder rather than using Windows'. |

The third is the sharpest, because the misattribution was *by our own alarm*
and it pointed at the one thing the whole spike existed to test. An alarm that
cries wolf about the hypothesis under test is worse than no alarm: the one time
it fires correctly, nobody believes it.

The fourth is still live. It cannot be fixed at the cause — browsers give no
certificate UI to a subresource — so it is handled entirely by the second half
of the rule: `ws_relay`'s contract tells consumers not to render 1006 as
"unreachable" without qualification, and names the distinguishing test.

### Why the two signatures are worse together

A confident optional and a misattributed failure compose into something neither
produces alone. Two bridges running at once: one instance's trust check blames
the certificate for a listener that never bound, while the other hands out a QR
that scans perfectly and leads nowhere. Nothing in either message mentions a
port. That is an hour spent debugging TLS for a port collision, and **neither
review predicted it** — both defects were found separately and the interaction
was found by accident.

### The recognition question

For any error message: **could the component printing this have caused it?**
If not, it is relaying someone else's failure, and the reader will act on where
it appeared rather than where it came from.

---

## 1. The player link needs a deadman

### Why

A headset going to sleep drops the TCP connection. Observed twice in one
session, both `WSAECONNRESET`, and during sleep the port stops answering
entirely.

With nothing downstream noticing, the sequence is: headset comes off → TCP
drops → the bridge stops receiving position → the phone holds the last position
it heard → and holds whatever output that position implies, indefinitely. The
user is not wearing the headset and not looking at the phone.

That is the same shape as a bug the web app spent a day fixing: **a link that
dies while the state depending on it keeps reading as live.** A state that lies
is a safety defect, not a display defect.

### What the capture establishes

| Question | Answer from `fixtures/` + the session log |
|---|---|
| Does DeoVR keep playing on a sleeping headset? | **No.** 59 s of silence advanced position 2.46 s; 371 s of silence moved it *backwards* 131.7 s. |
| Is a reconnect detectable? | **Yes, and it is the only marker.** Every discontinuity was bracketed by a reconnect; there were none on a live connection. |
| Does the first packet after a reconnect look different? | **No.** Shape-identical to steady state — same five keys, no handshake. |
| How fast does the bridge notice? | **~3 s.** The probe reports the port unreachable, which is a positive signal rather than inferred silence. |

### The invariant this design rests on

> **A reconnect is a discontinuity. A live connection is continuous.**

Everything below depends on that. It is not a behaviour to be tuned; it is the
premise. State it that way so that if anyone later observes a position jump
**on a live connection**, they know they have falsified the premise rather than
found an edge case to special-case around.

The evidence for it is one player and one session: every discontinuity was
bracketed by a reconnect, and there were none in band. Strong, and not
unlimited.

The corollary is that no inference from position is needed — position only
corroborates. Which matters, because position across a reconnect went
**backwards 131.7 s** in this capture. A detector built on position alone would
have looked defensible right up until it jumped a script back two minutes,
straight into the unramped-resume behaviour recorded in
`docs/follow-ups/resume-ramp.md`.

### Partly built: the relay keepalive

The half of this that a downstream consumer could not work around itself has
landed, because a PWA client was about to depend on something that was not
true.

Its assumption was that the relay pushes once per player packet, so a paused
player produces ~1 message/s and silence therefore means the bridge is gone.
Measured, that assumption splits in two:

- **A paused player does still push.** `send_modify` notifies unconditionally
  even when the closure changes nothing, so a repeated identical packet reaches
  the phone. Asserted in `http.rs`'s tests rather than trusted to tokio's
  documentation, because a safety decision rests on it.
- **But packet count is not push count, and the gap is unbounded.** State
  crosses a `watch`, which keeps one slot. A consumer that is not polling —
  backgrounded tab, congested link, stalled render — collapses any number of
  updates into a single delivery. Measured: 500 updates, one wake. **There is
  no deadline that can be sized against that**, so the honest answer was not to
  tighten one.

So the relay now resends the current snapshot every
[`RELAY_KEEPALIVE`](src/http.rs) (1 s, matching the player's observed cadence)
whenever nothing has changed, and the timer resets on every change-driven send.
That makes "the bridge is alive" independent of "the player produced traffic",
which is the property a consumer actually needs, and it means silence has
exactly one cause. The full may/may-not contract is on `ws_relay`.

What remains unbuilt is everything below: the phone's own failure detection,
the three-state distinction on the client side, and the resume policy.

### Proposal

- **Publish the port-unreachable probe result.** This is the best signal in the
  system and it already exists: within ~3 s of a headset sleeping, the probe
  reports the port unreachable. A *positive* "the player is gone" beats
  anything inferred from silence, and it arrives sooner than the two frames of
  position that a staleness rule would need. Publish it rather than leaving the
  phone to notice an absence.
- **Publish a staleness deadline as well**, for the case the probe misses —
  a connection that stays open while the player stops talking. Frames arrive at
  ~1010 ms; silence beyond a small multiple is a dead source, not a slow one.
  Going quiet is the failure mode, not the signal.
- **Three states, not one.** The phone must react differently to: player
  *paused* (position static, link alive), player *gone* (link dropped or port
  unreachable), bridge *unreachable* (the phone's own WebSocket died). A single
  "no data" collapses three situations with different correct responses. All
  three now fall out of the protocol and the probe rather than needing to be
  invented.
- **The phone must fail safe on its own.** If the bridge process is killed, no
  message arrives to say so. A closing WebSocket is one signal; a heartbeat the
  phone times out on is the one that survives a wedged bridge holding the
  socket open.
- **Reconnect after a gap should re-establish, not resume.** Open question for
  the user, because it interacts with the resume ramp.

### The design fork, and a recommendation

Should the bridge derive and publish `playing`, or publish raw frames and let
each consumer derive it?

**The bridge should derive it.** The rule — position advancing is
authoritative, `playerState` is advisory — is subtle enough that every consumer
re-deriving it would produce several slightly different, slightly wrong
versions. Deriving once means the PWA, the desktop app after it is hollowed
out, and anything later all inherit the same reading. The bridge should keep
publishing raw frames as well, which it already does, so a consumer that
disagrees can override rather than being stuck with our judgement.

Note the latency this inherits: a headset-initiated pause is undetectable for
**1.0–2.0 s** (two packets at ~1010 ms). Remote-initiated pauses are immediate,
because remote-set is exactly what `playerState` echoes.

---

## 1a. Presence is an open question, and the protocol cannot answer it

Not a task. A constraint, stated so nobody spends a day trying to solve it in
the bridge.

The captured key set is exactly `path`, `duration`, `currentTime`,
`playbackSpeed`, `playerState`. There is no worn/unworn field, and there is no
reason to expect one — remote control is a playback protocol.

So **"the video is playing" does not imply "the user is present and engaged"**,
and every design so far has quietly assumed it does. A headset can sit on a
desk with playback running. Deciding what the app should require before it
keeps driving output is a product decision, and it belongs to the user; any
answer has to come from a signal outside this protocol entirely.

One candidate, with its objection attached rather than as a recommendation:
**the controller**, since the user is holding it and the desktop app already
samples it at 60 Hz. The objection is that holding still is not absence — a
naive idle timeout would cut output on someone lying perfectly still, which is
a plausible thing to be doing.

---

## 2. TLS — built. What is left is verification, not design

The local CA this section used to propose was implemented: `certs.rs`,
`tls.rs`, `install.rs` and `mdns.rs`, with the install page on the plain-HTTP
listener. See the README section "The phone needs HTTPS".

What was **measured**, rather than assumed:

| Claim | How it was checked |
|---|---|
| `coyote.local` resolves to the LAN address | Live responder; `ping coyote.local` → `192.168.0.9`, the Ethernet address rather than the WSL adapter |
| Windows' own responder is not usable for this | `ping JUSTIN-G.local` → `172.21.160.1`, a virtual switch a phone cannot reach |
| The certificate satisfies Apple's rules | `openssl` against the running listener: SAN present, `id-kp-serverAuth`, 365-day validity, ECDSA P-256 |
| A strict client accepts it | `curl --cacert` and a rustls client both complete the handshake; both are refused without the CA |
| `wss://` works on the TLS listener | Integration test. A secure page cannot open `ws://`, so this is the acceptance path, not a nicety |
| The CA survives a restart | Second run logs "using the existing local CA"; the PEM is byte-identical |

### Verified on a real iPhone, 2026-07-29

The acceptance condition was never "the certificate is well-formed" — it was
**does Web Bluetooth work from the resulting origin**. It does.

| Step | Result |
|---|---|
| `coyote.local` resolves from the phone | **Yes** |
| Profile downloads and installs in Safari | **Yes** |
| Certificate Trust Settings toggle appears and works | **Yes** |
| Safari reports the origin trusted | **Yes** |
| **Bluefy honours the system-store CA** | **Yes** |
| **Coyote connected over Bluetooth from `https://coyote.local:8443`** | **Yes** |

Scope it as tightly as the DeoVR capture is scoped: **one handset, one iOS
version, one Bluefy version, one network, once.** That is enough to build on
and not enough to call universal. In particular, that Bluefy honours the iOS
system trust store is an observation about the build that was installed that
evening, not a documented guarantee from its author.

**The mDNS responder is why any of this worked.** Windows' own responder was
measured advertising `172.21.160.1` — the WSL virtual switch — for this
machine's `.local` name. A phone handed that address cannot reach anything, and
the failure would have presented as a certificate problem, because that is what
every visible symptom points at. If this crate is ever refactored, the responder
is not an optimisation to be dropped in favour of the OS.

### The failure mode to check first when someone else's phone does not work

Unexercised now rather than wrong, and still the right first suspicion:

> An untrusted `wss:` subresource fails **silently**. Certificate UI only exists
> for top-level navigations, so the socket closes as code 1006 with no
> interstitial and nothing the page can inspect. It reaches the user as "bridge
> unreachable", which is indistinguishable from an unplugged router.

The distinguishing test is whether the same origin loads in a top-level tab —
which is exactly what `/secure-check` is for. **If that page loads, the
certificate is trusted and the network is fine**, so a failing socket is about
trust and nothing else.

### Still open

- **Revocation has no command yet, and the half-built version is a safety
  defect.** `DeviceStore::revoke` deletes the credential and
  `ClientRegistry::revoke` closes the live sockets. Neither PR wires either to
  an IPC command, so there is currently no button — and the way this ships wrong
  is somebody adding the obvious one that calls the store and not the registry.

  That failure is silent and it is the dangerous direction: the record is gone,
  the panel says revoked, the user believes the device is disconnected — and the
  phone keeps its relay and **keeps driving output** until the network happens
  to drop it. Deleting a record stops the next connection; it does nothing to
  the one that is running.

  Both calls, store first, so a reconnect in the gap is refused rather than
  re-admitted:

  ```rust
  state.devices.revoke(&id)?;              // durable before we go on
  let closed = state.clients.revoke(&id);  // and stop what is running now
  ```

  Owned by whoever merges second, together with installing the credential
  resolver — the two are the same wiring job.

- **Token exposure on the first hop.** The QR points at plain HTTP by
  necessity, so the pairing token is readable by anyone on the LAN at that
  moment. Per-device credentials narrow what that buys — a sniffer can pair a
  device of their own, which is visible in the clients panel and individually
  revocable, rather than silently becoming the same principal as the phone —
  but it does not close it.

- **Whether iOS partitions the Home Screen cookie jar. UNTESTED, and it decides
  a real behaviour.** If an installed web app gets storage separate from the
  Safari tab that paired, launching from the home screen arrives with no
  credential and must pair again.

  The obvious fix — putting the token in the manifest's `start_url` — was built
  and then **deliberately removed**, because it lets a revoked device silently
  re-pair itself on its next launch. That defeats the revocation the whole
  per-device design exists to provide, and a speculative convenience is not
  worth a hole in a security property. If partitioning turns out to be real, the
  answer is to make the home-screen launch pair explicitly and visibly, not to
  hide a standing token in the manifest.

  Testing it costs one home-screen install on the handset that already works.
- **Renewal has never been observed.** The re-issue path is exercised by unit
  tests against synthetic timestamps, not by a bridge that has run for a year or
  had its DHCP lease move underneath it.
- **HereSphere, media changes, on-device media** — unchanged from the spike.

### The documented alternative

**Tailscale** gives a real certificate on a stable `*.ts.net` hostname via
`tailscale cert` / Serve — no certificate to install on the phone, and it works
off-LAN. It costs a dependency and an account on both machines.

It is no longer the contingency, because the local CA works. It remains the
answer for anyone who cannot or will not install a root certificate on their
phone, which is a legitimate position and not one to argue with.

**Cloudflare quick tunnels are the wrong default.** The hostname churns every
restart, which is a new origin every launch, which makes the PWA amnesiac —
that is the difference between an app and a demo, not a polish issue.

---

## 3. The funscript library: what was deliberately left out

`src/library.rs` serves `/library/index.json` and `/library/<name>`. Three
things it does not do, each a decision rather than an omission, with the number
that would force it.

- **No subdirectories.** The scan is flat. Recursion is where symlink loops,
  unbounded depth and a name that is no longer a single path segment all arrive
  together, and the client matches on a bare filename anyway. Revisit with a
  depth cap and a visited-inode set — not by removing the `components().count()
  == 1` check, which is one of the four gates on the fetch path.
- **No pagination, and the number is 673 KB.** That is a measured 10,000-entry
  index, sent whole on every fetch. The scan itself is 18-21 ms and is not the
  problem. `generation` is already the etag-shaped field to hang a conditional
  fetch on when someone's library gets there.

- **`set_library_dir` does not restart the poller.** It writes the setting and
  takes effect next launch, exactly like `set_static_dir` — so between the call
  and a restart, `bridge_status` reports the new path while
  `/library/index.json` still answers for the old one. Two sources of truth for
  one question, which is §0 again. Harmless only because **no UI calls it yet**;
  whoever builds one must either restart the poller or label the field as
  pending. It is currently dead code kept deliberately, so the window has
  something to call when it grows a folder picker.
- **No `canonicalize`.** Inherited from `serve_static`'s known gap, and shared
  with it deliberately so there is one implementation to fix. A symlink named
  `*.funscript` inside the library root, pointing outside it, is listed and
  served. Placing one requires write access to a directory the user chose — at
  which point the attacker can put the file there instead — and symlinked
  collections are how media libraries are actually assembled. If this is ever
  closed, close it in `http::safe_relative_path` so both callers get it.

  Worth knowing how this paragraph nearly shipped as fiction. The first version
  of `scan` used `DirEntry::metadata`, which is `lstat` on every platform, so
  symlinks were silently *excluded* — while this file asked a reviewer to accept
  a risk that did not exist, and the code broke the exact layout the prose
  called legitimate, with no log line. Two wrong statements pointing in opposite
  directions, neither visible from the other. A test now asserts the behaviour
  rather than the comment claiming it.

  One residue, deliberately open: an indexed regular file replaced by a symlink
  before it is fetched is followed, because `File::open` follows and the index
  is up to `DIR_POLL` old. Same write-access bound, so the disposition is the
  same.

The one thing that would be a defect rather than a limit: **`serve_static` still
does not percent-decode**, so the PWA cannot have an asset with a space in its
name. `library::percent_decode` is the decoder to reuse; the reason it was not
wired in here is that doing so means auditing every static path at the same
time, which is a different change.
