# Bridge follow-ups

Two pieces of work this session identified but deliberately did not build, plus
the evidence that scopes them. Both are sessions of their own; half-building
either would have been worse than writing them down.

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

## 2. TLS, so the phone can use Web Bluetooth

### Why

Web Bluetooth requires a secure context. `localhost` is exempt, which is
precisely what hides this during desktop testing — the phone is not localhost.
Everything else in the app works fine over plain HTTP, so **HTTPS is required
for the Coyote connection specifically, not for the app in general**, and the
plain-HTTP path should stay because it is the easiest thing to debug.

A design decision already settled says the bridge serves the PWA itself — no
version skew, users can modify their own instance, no central dependency. The
bridge owning TLS is the coherent consequence of that, not an add-on.

### Chosen approach: a local CA, delivered through the existing QR flow

Bridge generates a CA on first run, keeps the private key local, and serves the
install page over plain HTTP. QR → install page → one tap → thereafter
`https://<stable-name>:8443` with a valid certificate, permanently. `rcgen`
does the certificate work in about thirty lines; the crypto is not the hard
part.

The hard parts, each of which needs handling explicitly:

1. **iOS needs two steps in two places, and everyone misses the second.**
   Installing the profile is Settings → Profile Downloaded → Install. The cert
   is **not trusted** until Settings → General → About → **Certificate Trust
   Settings** → enable full trust for the root. Skipping it fails
   indistinguishably from a broken certificate. This cannot be automated, so
   the install page must carry numbered steps with the exact path — and a
   **"check my trust" button** that attempts an HTTPS fetch and reports yes or
   no. A verification step that gives a clear answer is worth more than any
   amount of instruction prose.
2. **The address must be stable.** An IP SAN works until DHCP moves them, and a
   changed address is a changed origin — the exact churn that rules out quick
   tunnels, because it wipes OPFS, kills the PWA install and resets the
   Bluetooth device grant. Prefer **mDNS `coyote.local`**, which iOS resolves
   natively; put a DNS SAN in the leaf and the current IP as an additional SAN
   for fallback, but put the hostname on the QR.
3. **Apple rejects naive self-signed certs** (iOS 13+): SAN required and CN
   ignored, `id-kp-serverAuth` in EKU, validity **≤398 days**, RSA ≥2048 or ECC
   P-256/384. Each failure is silent about its cause. Verify against a real
   iPhone before claiming it works.
4. **Persist the CA.** A regenerated CA means a reinstall every launch. Store it
   with the existing config and treat loss as a user-visible event rather than
   silently minting a new one.
5. **Renew ahead of expiry.** Re-issue the leaf from the stored CA on startup
   when it is near expiry; the phone never reinstalls.

### Security — not negotiable, and this is going open source

Installing a root CA lets that CA sign a certificate for **any** domain.
Whoever holds the key can impersonate anything to that phone.

- **Generate the CA per-install, on the user's own machine.** Never ship a CA
  key in the binary. A shipped key would let anyone who downloaded the release
  MITM every user who ever installed it — catastrophic and unfixable after the
  fact.
- **The private key never leaves the machine.** Not in the QR, not over the
  network, not in logs. The install page serves the **public** certificate only.
- Name the certificate so it is identifiable in a Settings list months later,
  and document removal.
- State the trade on the install page rather than burying it. People should
  know what they are granting.

### The documented alternative

**Tailscale** gives a real certificate on a stable `*.ts.net` hostname via
`tailscale cert` / Serve — no domain to buy, no cert to install on the phone,
and it works off-LAN. It costs a dependency and an account on both machines.
Worth knowing about for anyone already running it; not worth building instead.

**Cloudflare quick tunnels are the wrong default.** The hostname churns every
restart, which is a new origin every launch, which makes the PWA amnesiac —
that is the difference between an app and a demo, not a polish issue.
