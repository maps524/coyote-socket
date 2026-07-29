//! Who is connected to the bridge — and, more importantly, how much of that we
//! actually know.
//!
//! # The question this answers
//!
//! "Is my phone actually talking to this?" Until now the only evidence was a
//! log line, so the answer was guessed at. This module keeps the guess out of
//! it: every WebSocket that reaches [`crate::http::ws_relay`] registers here,
//! and what it registered is rendered in the desktop window and served from
//! `/healthz` so the headless build is not blind.
//!
//! # The hard part: a reconnect is not a second device
//!
//! A phone that roams between access points drops its socket and opens a new
//! one. That is **one** client. A phone and a tablet are **two**. Getting this
//! wrong in the confident direction — showing "2 connected" when a phone
//! roamed — is worse than showing nothing, because it is a number someone will
//! act on.
//!
//! So it is worth being exact about what identity is available:
//!
//! | Signal | Why it is not identity |
//! |---|---|
//! | Peer address | The port is fresh on every connection. The IP changes on a roam or a DHCP lease renewal, and NAT collapses a whole household behind one. Two devices can share it; one device can present three. |
//! | Pairing token | Deliberately shared. `auth.rs` says so in its own words: "everyone who has the token is the same principal". A second phone pairs by scanning the *same* QR. |
//! | `User-Agent` | A device *class* at best. Two identical iPhones are byte-identical here, and it is trivially forged. |
//! | `Sec-WebSocket-Key` | Random per connection. Actively anti-identity. |
//!
//! That list is exhaustive for what the bridge can observe *on its own*, and
//! none of it identifies anything. Identity has to be something the client
//! carries, and there are two grades of it — see [`Provenance`]:
//!
//! - **A verified per-device credential.** Minted by the pairing flow: the
//!   token on the QR is exchanged once, over the HTTPS origin, for a
//!   credential the browser keeps and presents on every later upgrade. This is
//!   the real answer. A roaming phone keeps it across a reconnect; a tablet has
//!   its own. NAT and DHCP stop mattering.
//! - **A self-reported id.** A string the client chose, on `?c=` or in a
//!   `hello`. Better than nothing and worth exactly what it costs to forge.
//!   It is the fallback for a client that has not paired yet — and the pairing
//!   token does not go away, so "un-credentialed" does not mean "stale", it may
//!   mean "mid-pairing".
//!
//! The two never merge: [`Key`] folds the provenance in, so a client cannot
//! claim its way onto someone else's credentialed row.
//!
//! # What even a credential does not tell you
//!
//! **It identifies a browser storage partition** — not a handset and not a
//! person. Consequences, all of them stated in the panel rather than only here:
//!
//! - Safari and a home-screen install on one phone may hold two credentials,
//!   and show as two.
//! - A browser whose storage was cleared is a new one, with no way to know it
//!   was the old one.
//! - Anyone holding the pairing token can obtain a credential.
//!
//! This is why [`BrowserCount`] counts *browsers*. Counting "devices" would be
//! asserting a headcount nothing here can support.
//!
//! # And when nothing is presented at all
//!
//! Then we say so. [`BrowserCount::AtLeast`] is what a view reports whenever
//! any connected client is unidentified, and the window renders it as a floor
//! with the reason attached, never as a count. `FOLLOW-UPS.md` section 0 gives
//! the shape of every defect this project has had from assuming a singleton:
//! *a field that promises a capability nobody has confirmed*, with the
//! optimistic reading as the default. A plain `devices: usize` would be exactly
//! that field, so there isn't one.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tokio::sync::watch;

use crate::logging::now_ms;

/// Query-string key a client may use to present its id on `/ws`.
///
/// A query parameter as well as a [`parse_hello`] message because the two
/// arrive at different times: the query is known before the socket is even
/// accepted, so the row is right from the first frame, whereas a `hello` can
/// only land after. A client that can do either should do both.
pub const CLIENT_ID_PARAM: &str = "c";

/// Shortest id we will accept.
///
/// Not a security control — nothing here is. It is a nudge against ids like
/// `phone`, which two devices would pick independently and then be merged into
/// one row, silently *under*-counting. Under-counting is the failure this
/// module exists to prevent, so the cheap guard is worth having.
const MIN_ID_LEN: usize = 8;
const MAX_ID_LEN: usize = 64;
const MAX_LABEL_LEN: usize = 40;
/// A `User-Agent` is a device hint, not a document. Cap it before it reaches a
/// JSON body and a window.
const MAX_AGENT_LEN: usize = 120;

/// How many disconnected clients to keep, and for how long.
///
/// Not history — the session's own recent past, which is what makes a
/// reconnect legible at all. A row that says "was here four seconds ago" is
/// the difference between reading a roam and inventing one. Bounded on both
/// axes so an afternoon of reconnects cannot grow the panel without limit.
const RETAIN_DISCONNECTED: usize = 8;
const RETAIN_DISCONNECTED_MS: u64 = 10 * 60 * 1000;

/// How recently a disconnect must have happened for a fresh unidentified
/// connection from the same address and agent to be *worth mentioning* as
/// possibly the same client. Never used to merge anything — see
/// [`ClientView::maybe_same_as`].
const REJOIN_HINT_MS: u64 = 60 * 1000;

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// Where an identity came from, and therefore how much it is worth.
///
/// Rendered in the panel, because "the bridge verified this" and "the client
/// asserted this" are different claims and only one of them survives an
/// adversary. Ordered weakest-first: a credential outranks a claim, so a
/// connection presenting both is keyed on the credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Provenance {
    /// The client sent an id it chose for itself. Unauthenticated: anyone
    /// holding the pairing token can present any id, and two clients that pick
    /// the same one are shown as one.
    SelfReported,
    /// The bridge verified a per-device credential. This is the identity that
    /// actually distinguishes a roam from a second device — it is carried in
    /// the client's own storage and survives a new address.
    Credential,
}

/// How a group of connections is keyed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    /// An identity, with the provenance folded into the key.
    ///
    /// The provenance is part of the key rather than a field beside it so that
    /// a self-reported id can never merge into a credentialed row. Without
    /// that, any client holding the pairing token could send
    /// `?c=<someone's credential id>` and appear as their phone — an identity
    /// panel that can be impersonated is worse than none.
    Identified { provenance: Provenance, id: String },
    /// The client presented nothing, so this connection stands alone. Keyed by
    /// its sequence number precisely so it **cannot** merge with anything: we
    /// have no basis on which to merge it, and inventing one is the bug.
    Anonymous(u64),
}

impl Key {
    fn render(&self) -> String {
        match self {
            Key::Identified {
                provenance: Provenance::Credential,
                id,
            } => format!("cred:{id}"),
            Key::Identified {
                provenance: Provenance::SelfReported,
                id,
            } => format!("c:{id}"),
            Key::Anonymous(seq) => format!("anon:{seq}"),
        }
    }

    fn provenance(&self) -> Option<Provenance> {
        match self {
            Key::Identified { provenance, .. } => Some(*provenance),
            Key::Anonymous(_) => None,
        }
    }
}

/// A per-device credential the bridge has verified.
///
/// Minted and stored by the pairing work in `bridge-tls`: the pairing token is
/// exchanged once, over the HTTPS origin, for a credential the browser keeps
/// and presents on every later upgrade. **The `id` here is not the secret** —
/// the bearer value is stored hashed and never leaves the bridge, while this id
/// is safe to render, log and put in a JSON body, which it is.
/// # Contract the resolver must satisfy
///
/// `id` is deliberately **not** run through [`sanitise_id`]: that guard exists
/// for strings a client chose, and applying it here would impose a length rule
/// on someone else's generator for no benefit. But it is not unconstrained
/// either, because it lands in a map key, a log line, a `/healthz` body and a
/// desktop window. A resolver must supply an `id` that is:
///
/// - **stable** for the life of the credential, and distinct per credential;
/// - **non-secret** — the bearer value is not this, and must never be;
/// - **bounded and printable** — no control characters, no newlines, and short
///   enough to render. The existing implementation supplies random hex.
///
/// Nothing enforces this at the boundary, because the resolver is installed by
/// the process itself rather than by a peer. It is a contract between two
/// modules in one binary, and it is written down so the second one to change
/// knows what the first assumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credential {
    pub id: String,
    /// A name the *user* gave this device. Persisted with the credential, not
    /// here: this registry is per-run and deliberately forgets everything on
    /// restart, and a label the user typed must outlive that.
    pub label: Option<String>,
    /// When the device first paired, from the credential store. Durable across
    /// restarts, unlike this registry's own first-seen, which only knows about
    /// the current run.
    pub created_ms: Option<u64>,
}

/// How a connection identified itself, at the moment it was accepted.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ConnectingIdentity {
    /// A verified per-device credential.
    Credential(Credential),
    /// A client-chosen id, with nothing behind it.
    Claimed(String),
    /// Nothing was presented.
    #[default]
    Unidentified,
}

/// Accept a client-supplied id, or refuse it.
///
/// Deliberately narrow: this string is rendered in a window, written to a JSON
/// body and used as a map key, and it arrives from an unauthenticated peer.
/// Length-bounded and restricted to characters that cannot be mistaken for
/// structure anywhere it lands.
pub fn sanitise_id(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.len() < MIN_ID_LEN || raw.len() > MAX_ID_LEN {
        return None;
    }
    raw.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
        .then(|| raw.to_string())
}

/// Accept a client-supplied display name, or refuse it.
///
/// Truncated, and stripped of anything that could break out of a line: this
/// text is chosen by whoever holds the token and is shown to whoever owns the
/// bridge.
pub fn sanitise_label(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_LABEL_LEN)
        .collect();
    let cleaned = cleaned.trim().to_string();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// Resolve a `Cookie` header into a verified device, if one is presented.
///
/// The one seam between this module and the per-device credentials in
/// `bridge-tls`. Installed with [`ClientRegistry::set_credential_resolver`] at
/// startup; until something installs one, every connection is judged on what
/// it says about itself, which is exactly the weaker world this panel already
/// describes honestly.
pub type CredentialResolver = Arc<dyn Fn(Option<&str>) -> Option<Credential> + Send + Sync>;

/// Pull `?c=…` out of a request target.
pub fn id_from_query(path_and_query: &str) -> Option<String> {
    let (_, query) = path_and_query.split_once('?')?;
    let raw = query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == CLIENT_ID_PARAM).then_some(value)
    })?;
    sanitise_id(raw)
}

/// What a client says about itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Claim {
    pub id: Option<String>,
    pub label: Option<String>,
}

impl Credential {
    /// Whether this id can safely key a row and be rendered.
    ///
    /// Deliberately far weaker than [`sanitise_id`] — no length floor, no
    /// charset — because this comes from the bridge's own store rather than
    /// from a peer, and imposing our format on someone else's generator would
    /// buy nothing. It rejects only what cannot be displayed at all: nothing,
    /// whitespace, control characters, or something too long to be a name.
    pub fn id_is_usable(&self) -> bool {
        let id = self.id.trim();
        !id.is_empty()
            && id.len() <= MAX_ID_LEN
            && !self.id.chars().any(|c| c.is_control())
    }
}

impl Claim {
    pub fn is_empty(&self) -> bool {
        self.id.is_none() && self.label.is_none()
    }
}

/// Parse a client→bridge `hello`.
///
/// The bridge sends a `hello` too, in the other direction; there is no
/// ambiguity because neither side reads its own. Returns `None` for anything
/// that is not a hello so the caller can fall through to command parsing.
pub fn parse_hello(text: &str) -> Option<Claim> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    if v.get("type")?.as_str()? != "hello" {
        return None;
    }
    Some(Claim {
        id: v
            .get("clientId")
            .and_then(|v| v.as_str())
            .and_then(sanitise_id),
        label: v
            .get("label")
            .and_then(|v| v.as_str())
            .and_then(sanitise_label),
    })
}

/// A coarse device hint from a `User-Agent`.
///
/// Named a hint because that is all it is. It cannot separate two identical
/// phones and it is trivially forged; its job is to make a row recognisable to
/// a human — "the iPhone" rather than "192.168.0.31" — not to identify
/// anything.
pub fn agent_hint(user_agent: &str) -> Option<String> {
    let ua = user_agent.trim();
    if ua.is_empty() {
        return None;
    }
    let platform = if ua.contains("iPhone") {
        Some("iPhone")
    } else if ua.contains("iPad") {
        Some("iPad")
    } else if ua.contains("Android") {
        Some("Android")
    } else if ua.contains("Windows") {
        Some("Windows")
    } else if ua.contains("Macintosh") || ua.contains("Mac OS") {
        Some("Mac")
    } else if ua.contains("Linux") {
        Some("Linux")
    } else {
        None
    };
    // Order matters: every Chrome UA also says Safari, and Edge says both.
    let browser = if ua.contains("Edg/") {
        Some("Edge")
    } else if ua.contains("Firefox/") {
        Some("Firefox")
    } else if ua.contains("Chrome/") || ua.contains("CriOS/") {
        Some("Chrome")
    } else if ua.contains("Safari/") {
        Some("Safari")
    } else {
        None
    };
    Some(match (platform, browser) {
        (Some(p), Some(b)) => format!("{p} · {b}"),
        (Some(p), None) => p.to_string(),
        (None, Some(b)) => b.to_string(),
        // Not a browser we recognise — show what it said, truncated. A native
        // client or a script lands here and is worth seeing verbatim.
        (None, None) => ua.chars().take(MAX_AGENT_LEN).collect(),
    })
}

// ---------------------------------------------------------------------------
// The rendered view
// ---------------------------------------------------------------------------

/// How many distinct browsers are connected — or why we will not say.
///
/// **Browsers, not devices, and the word is doing work.** A per-device
/// credential lives in one browser's storage partition, so Safari and a
/// home-screen install on the same handset can hold two of them, and a browser
/// whose storage was cleared holds a new one. Counting "devices" would be
/// asserting a headcount that neither this module nor the credential store can
/// support; counting browsers is exactly what the evidence shows.
///
/// Two variants rather than a bare number, for the reason `FOLLOW-UPS.md`
/// gives: a single `usize` here would be a field promising knowledge nobody
/// confirmed, and its optimistic reading would be the default.
///
/// Even [`Self::Reported`] is not certainty — it is the count of distinct ids
/// presented, and the name says so.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum BrowserCount {
    /// Every connected client presented an id, so this is the number of
    /// distinct ids. A browser that cleared its storage still counts as new.
    ///
    /// `provenance` says what that number is worth, and it is not decoration.
    /// `Reported` would otherwise mean both "every one of these was verified by
    /// the bridge" and "every one of these is a string a client typed", which
    /// are not the same claim and must not render the same. The weakest
    /// provenance present governs: one self-reported client among credentialed
    /// ones makes the whole count self-reported, because the count is only as
    /// trustworthy as its softest member.
    ///
    /// This gets sharper once pairing lands. A mixed view would otherwise carry
    /// the credentialed footnote — "a verified row is a browser" — over a
    /// number partly composed of ids anyone could have sent.
    Reported {
        count: usize,
        provenance: Provenance,
    },
    /// At least one connected client presented nothing, so `count` is a floor
    /// and nothing more. `unidentified` says how many connections we cannot
    /// attribute; each of them is somewhere between "a browser we have not
    /// seen" and "the one that just roamed".
    AtLeast { count: usize, unidentified: usize },
}

/// One identity group, connected or recently gone.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientView {
    /// Stable for as long as this row exists. For rendering keys, not identity.
    pub key: String,
    /// Whether this row rests on any identity at all.
    pub identified: bool,
    /// Where that identity came from. `None` when there is none.
    ///
    /// The panel renders this rather than treating all identity as equal: a
    /// verified credential and a string the client typed are not the same
    /// claim, and only one of them survives someone trying.
    pub provenance: Option<Provenance>,
    /// The identity, when there is one. For a credential this is the
    /// non-secret half — never the bearer value.
    pub id: Option<String>,
    /// A name for this row: set by the user on a credential, chosen by the
    /// client on a self-reported one.
    pub label: Option<String>,
    /// When this device first paired, from the credential store. `None` for
    /// anything without a credential, and for a credential store that does not
    /// record it.
    pub created_ms: Option<u64>,
    pub connected: bool,
    /// Open sockets under this identity. More than one is normal — a second
    /// browser tab is a second socket on the same device.
    pub sockets: usize,
    /// How many times this identity has connected during this bridge run.
    /// Greater than one on an identified row is a reconnect, stated as a fact
    /// rather than inferred by whoever is reading the panel.
    pub connections: u32,
    pub first_seen_ms: u64,
    /// When the current — or, for a gone row, the last — connection opened.
    pub connected_at_ms: u64,
    pub disconnected_at_ms: Option<u64>,
    /// When a frame last arrived **from** this client.
    ///
    /// Read that literally, because an earlier version did not. This used to be
    /// set after a successful *write*, and a successful write means only that
    /// the local TCP stack accepted some bytes — nothing about the peer. A
    /// phone carried out of range holds a half-open socket, and 200-byte
    /// snapshots at 1 Hz take minutes to fill a send buffer, so the field named
    /// "last heard" could not detect the one case it existed for. It was a
    /// diagnostic pointing at the wrong subsystem, which `FOLLOW-UPS.md` §0b
    /// names as its own defect signature.
    ///
    /// It is now genuine evidence: the relay sends a WebSocket Ping each
    /// keepalive and every browser answers with a Pong, so a live client
    /// refreshes this about once a second. Silence here is the client's
    /// silence.
    ///
    /// **Stale still implies dead; fresh does not imply alive** in the strict
    /// sense — a Pong is answered by the browser, not by the page's script, so
    /// this proves the browser is reachable rather than that the app is
    /// healthy. That is a much smaller gap than the one it replaces, and it is
    /// the strongest claim available without an application-level heartbeat.
    pub last_heard_ms: u64,
    /// The peer address of the most recent socket.
    pub address: String,
    /// Earlier addresses this identity has connected from. A non-empty list on
    /// an identified row is a roam, observed rather than assumed.
    pub previous_addresses: Vec<String>,
    pub agent: Option<String>,
    /// A *guess*, and labelled as one wherever it is shown.
    ///
    /// Set on an unidentified row when a different unidentified row from the
    /// same address and the same agent disconnected moments ago. That is a
    /// plausible roam or refresh, and saying "this may be the client that just
    /// dropped" is more useful than saying nothing.
    ///
    /// **It never affects [`BrowserCount`].** The counts stay pessimistic; only
    /// the prose speculates. Merging on this evidence is precisely the
    /// confidently-wrong number this module exists to avoid.
    pub maybe_same_as: Option<String>,
    /// This row's id is one a verified credential is also using.
    ///
    /// Only ever set on a self-reported row: a client volunteered an id that
    /// belongs to a device this bridge has actually verified. Keying on
    /// provenance already keeps the two apart, so the counts are right without
    /// this — but both rows render the same id string, and the panel is what
    /// someone reads before deciding which row to revoke. Two identical-looking
    /// ids with nothing to separate them is how the wrong one gets picked.
    pub impersonating: bool,
    /// Whether "revoke this device" is a coherent action on this row.
    ///
    /// True only for a verified credential that has not already been revoked:
    /// there is something durable to delete, and deleting it means the device
    /// cannot come back. Revoking a self-reported id would close a socket that
    /// reconnects a second later under any id it likes, which is a button that
    /// appears to work and does not — the failure mode this whole panel exists
    /// to stop shipping.
    pub revocable: bool,
    /// When this device was revoked, if it was, during this bridge run.
    ///
    /// The row outlives the sockets by minutes, and those are exactly the
    /// minutes in which someone is looking at the panel to check the revoke
    /// worked. Without this it would still read "verified" — a credential that
    /// no longer exists, described on the screen that is supposed to confirm
    /// its removal.
    pub revoked_at_ms: Option<u64>,
    /// A revoked device reconnected with a credential that still verifies.
    ///
    /// **This is a detector, not a state.** It can only happen when the
    /// credential was never deleted — the half of revocation that lives in the
    /// credential store. Closing the socket without deleting the record leaves
    /// a device that comes back, and the panel would otherwise show it as
    /// "revoked" with a live socket attached, which is the worst reading in
    /// this whole module: reassuring, on the confirmation screen, about a
    /// phone that is at that moment driving hardware.
    ///
    /// The window renders it as a fault with what to do about it, rather than
    /// letting the two contradictory facts sit side by side for a reader to
    /// notice.
    pub revoke_contested: bool,
}

/// Everything the panel and `/healthz` render.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientsView {
    /// Open WebSockets. **This one we know exactly** — it is a count of
    /// sockets, which is a thing the bridge holds, not a thing it infers.
    pub connections: usize,
    pub browsers: BrowserCount,
    /// Connected first, most recent first; then recently disconnected.
    pub clients: Vec<ClientView>,
    /// True when at least one connected client presented no id. The window
    /// uses it to decide whether to explain itself.
    pub any_unidentified: bool,
    /// Whether per-device credentials are available at all on this bridge.
    ///
    /// False means nothing has installed a resolver, so every identity here is
    /// self-reported at best. The window says which world it is in rather than
    /// leaving the reader to infer it from an absence — the same reason the
    /// bind result is three-valued.
    pub credentials_available: bool,
}

// ---------------------------------------------------------------------------
// The registry
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Conn {
    seq: u64,
    address: String,
    connected_at_ms: u64,
    /// Written by the relay when a frame arrives from the client, without
    /// taking the registry lock — a per-second lock and a per-second UI
    /// notification for a clock would be a poor trade.
    last_heard_ms: Arc<AtomicU64>,
    /// Fires when this socket must close now.
    ///
    /// A revoked device has to *stop*, not fail its next connect: a phone that
    /// keeps driving output for the rest of the session after being un-paired
    /// is the worst available reading of the word "revoke". The relay selects
    /// on this and sends a close frame saying why.
    close: watch::Sender<bool>,
}

#[derive(Debug)]
struct Record {
    key: Key,
    label: Option<String>,
    agent: Option<String>,
    created_ms: Option<u64>,
    /// Set by [`ClientRegistry::revoke`]. Survives the sockets closing, so the
    /// gone row says what happened to it rather than looking like a device
    /// that merely wandered off.
    revoked_at_ms: Option<u64>,
    /// Set when a revoked row is attached to again by a still-valid
    /// credential. See [`ClientView::revoke_contested`].
    revoke_contested: bool,
    first_seen_ms: u64,
    connected_at_ms: u64,
    disconnected_at_ms: Option<u64>,
    /// Last value read off the open sockets before they closed, so a gone row
    /// still says when it was last heard from.
    last_heard_ms: u64,
    connections: u32,
    address: String,
    previous_addresses: Vec<String>,
    open: Vec<Conn>,
}

#[derive(Debug, Default)]
struct Inner {
    next_seq: u64,
    records: BTreeMap<Key, Record>,
    /// Which record each open connection currently belongs to. A connection
    /// can move between records exactly once — when a `hello` names it.
    placement: BTreeMap<u64, Key>,
    /// Connections whose `hello` has already been applied.
    ///
    /// Enforces the "exactly once" above. Without it a client could re-identify
    /// in a loop, hopping identities and waking the desktop panel on every
    /// message. Cleared on disconnect with everything else about the socket.
    identified: std::collections::BTreeSet<u64>,
}

/// Live connections to the bridge's WebSocket relay.
///
/// One per bridge instance, shared by the HTTP surface (which registers
/// connections) and the front end (which renders them). Constructed by the
/// process that owns the listener, not by `http::run`, so the window can show
/// an empty-but-correct panel before the port is even bound.
pub struct ClientRegistry {
    inner: Mutex<Inner>,
    /// Bumped whenever the *set* of clients changes — a connect, a disconnect,
    /// an identification. Deliberately not bumped by [`ClientHandle::heard`],
    /// which fires once a second per client and would turn a status panel into
    /// a repaint loop.
    version: watch::Sender<u64>,
    /// Turns a `Cookie` header into a verified device, when per-device
    /// credentials exist on this build. `None` is not a failure state: it is
    /// the world before pairing was built, and the panel says which world it is
    /// in rather than letting an absence read as "nobody is credentialed".
    credentials: Mutex<Option<CredentialResolver>>,
}

impl std::fmt::Debug for ClientRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientRegistry")
            .field("connections", &self.view().connections)
            .finish()
    }
}

impl Default for ClientRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ClientRegistry {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            version: watch::channel(0).0,
            credentials: Mutex::new(None),
        }
    }

    /// Install the per-device credential resolver.
    ///
    /// Called once at startup by whoever owns the credential store. Keeping
    /// this here rather than on `http::Ctx` is deliberate: `http.rs` is
    /// contended, and this way the whole feature costs it one call site.
    pub fn set_credential_resolver(&self, resolver: CredentialResolver) {
        *self.credentials.lock().unwrap_or_else(|e| e.into_inner()) = Some(resolver);
    }

    /// Whether this bridge can verify a device at all.
    pub fn credentials_available(&self) -> bool {
        self.credentials
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    /// Decide who a connecting client is.
    ///
    /// **The one place identity is established**, so that improving it is a
    /// change here and nowhere else. A verified credential wins over a
    /// self-reported id, because a client that presents both is telling us one
    /// thing we checked and one thing we did not.
    pub fn identity_for(
        &self,
        path_and_query: &str,
        cookie_header: Option<&str>,
    ) -> ConnectingIdentity {
        let resolver = self
            .credentials
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(resolver) = resolver {
            if let Some(credential) = resolver(cookie_header) {
                // The contract on `Credential` cannot be enforced at a type
                // boundary, so it is enforced here. A blank or unprintable id
                // keys a row that renders as an empty name, and "the verified
                // device called nothing" is a worse answer than "we could not
                // establish who this is".
                //
                // Refuses rather than repairs: a store handing back a malformed
                // id is a bug in the store, and quietly making it presentable
                // would hide it. Loud, and falls back to the honest state.
                if credential.id_is_usable() {
                    return ConnectingIdentity::Credential(credential);
                }
                crate::log_warn!(
                    "[clients] the credential store returned an unusable device id                      ({} chars); treating this connection as unidentified",
                    credential.id.chars().count()
                );
            }
        }
        match id_from_query(path_and_query) {
            Some(id) => ConnectingIdentity::Claimed(id),
            None => ConnectingIdentity::Unidentified,
        }
    }

    /// Notifies on every change to the set of clients. The desktop app pumps
    /// this into a Tauri event so the panel is pushed, not polled.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.version.subscribe()
    }

    /// Record a newly accepted WebSocket.
    ///
    /// Returns a handle whose `Drop` deregisters the connection, so an early
    /// return, a panic or a torn socket cannot leave a phantom client on the
    /// panel — which would be the same class of lie as the count this module
    /// refuses to guess at.
    pub fn connect(
        self: &Arc<Self>,
        address: SocketAddr,
        user_agent: Option<&str>,
        identity: ConnectingIdentity,
    ) -> ClientHandle {
        let now = now_ms();
        let last_heard = Arc::new(AtomicU64::new(now));
        let close = watch::channel(false).0;
        let closed = close.subscribe();
        let seq = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let seq = inner.next_seq;
            inner.next_seq += 1;
            let (key, label, created_ms) = match identity {
                ConnectingIdentity::Credential(credential) => (
                    Key::Identified {
                        provenance: Provenance::Credential,
                        id: credential.id,
                    },
                    credential.label,
                    credential.created_ms,
                ),
                ConnectingIdentity::Claimed(id) => (
                    Key::Identified {
                        provenance: Provenance::SelfReported,
                        id,
                    },
                    None,
                    None,
                ),
                ConnectingIdentity::Unidentified => (Key::Anonymous(seq), None, None),
            };
            let conn = Conn {
                seq,
                address: address.to_string(),
                connected_at_ms: now,
                last_heard_ms: Arc::clone(&last_heard),
                close,
            };
            let agent = user_agent.and_then(agent_hint);
            inner.attach(key.clone(), conn, agent, label, created_ms, now);
            inner.placement.insert(seq, key);
            inner.prune(now);
            seq
        };
        self.bump();
        ClientHandle {
            registry: Arc::clone(self),
            seq,
            last_heard,
            closed,
        }
    }

    /// Close every live socket belonging to a revoked credential.
    ///
    /// Returns how many were closed. **`0` is not a failure, and it is not one
    /// state either** — it means "no live socket here", which covers a device
    /// that is merely offline, a device paired yesterday that this run has
    /// never seen (the registry is per-run and remembers nothing across a
    /// restart), and an id that does not exist. A caller must take its
    /// success from the credential store, which is durable and authoritative;
    /// this number is only "how much was disconnected just now".
    ///
    /// **Only credentialed rows** — revoking a self-reported id would close a
    /// socket that reconnects a second later under any id it likes, and a
    /// revoke button that appears to work and does not is worse than no
    /// button.
    ///
    /// This is half of revocation. The other half — deleting the credential so
    /// it cannot be presented again — belongs to the credential store, must
    /// happen *first*, and must survive the settings race that let a revoked
    /// token come back from the dead. Calling only this one leaves a device
    /// that reconnects immediately; calling only the other leaves a live socket
    /// driving hardware until it happens to drop.
    pub fn revoke(&self, id: &str) -> usize {
        let key = Key::Identified {
            provenance: Provenance::Credential,
            id: id.to_string(),
        };
        let mut known = false;
        let closed = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            match inner.records.get_mut(&key) {
                Some(record) => {
                    for conn in &record.open {
                        // The relay sees this and sends a close frame that says
                        // why. A silent drop is indistinguishable from a dead
                        // network, which is the exact ambiguity this project
                        // has spent a day removing.
                        let _ = conn.close.send(true);
                    }
                    // Mark the row, not just the sockets.
                    //
                    // The sockets close within milliseconds and the row then
                    // lingers as "recently disconnected" for minutes — which is
                    // the window in which someone is looking at this panel,
                    // because they just pressed revoke. Left alone it would go
                    // on wearing its "verified" tag for a credential that no
                    // longer exists, on the one screen that answers "did that
                    // work?". Historically accurate and actively misleading is
                    // the same trade this module refused everywhere else.
                    record.revoked_at_ms = Some(now_ms());
                    known = true;
                    record.open.len()
                }
                None => 0,
            }
        };
        // Bump when a row changed, including one with no open socket — its
        // meaning changed and the panel is being watched right now. Not for an
        // id this run has never seen, which is an ordinary outcome (the
        // registry is per-run) and changes nothing to repaint.
        if known {
            self.bump();
        }
        if closed > 0 {
            crate::log_info!("[clients] revoked device closed {closed} live socket(s)");
        }
        closed
    }

    /// The current picture, rendered.
    pub fn view(&self) -> ClientsView {
        let now = now_ms();
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());

        let connections: usize = inner.records.values().map(|r| r.open.len()).sum();
        let unidentified: usize = inner
            .records
            .values()
            .filter(|r| !r.open.is_empty() && matches!(r.key, Key::Anonymous(_)))
            .map(|r| r.open.len())
            .sum();
        let identified: usize = inner
            .records
            .values()
            .filter(|r| !r.open.is_empty() && matches!(r.key, Key::Identified { .. }))
            .count();
        // The weakest identity among the connected rows. `Provenance` is
        // ordered weakest-first, so this is a min.
        let weakest = inner
            .records
            .values()
            .filter(|r| !r.open.is_empty())
            .filter_map(|r| r.key.provenance())
            .min()
            .unwrap_or(Provenance::Credential);

        let browsers = if unidentified == 0 {
            BrowserCount::Reported {
                count: identified,
                provenance: weakest,
            }
        } else {
            // The floor: every presented id is certainly a browser, and the
            // unidentified connections are somewhere between zero further
            // browsers (all of them roams of clients already listed) and one
            // each. We do not pick.
            BrowserCount::AtLeast {
                count: identified,
                unidentified,
            }
        };

        let mut clients: Vec<ClientView> = inner
            .records
            .values()
            .map(|r| r.render(&inner, now))
            .collect();
        // Connected first, then by recency. The panel is read top-down when
        // someone is asking "is my phone on?".
        clients.sort_by(|a, b| {
            b.connected
                .cmp(&a.connected)
                .then(b.connected_at_ms.cmp(&a.connected_at_ms))
        });

        ClientsView {
            connections,
            browsers,
            clients,
            any_unidentified: unidentified > 0,
            credentials_available: self
                .credentials
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some(),
        }
    }

    fn bump(&self) {
        self.version.send_modify(|v| *v += 1);
    }

    /// Move a connection onto a claimed identity, or update its label.
    fn identify(&self, seq: u64, claim: Claim) {
        let now = now_ms();
        let changed = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.identify(seq, claim, now)
        };
        if changed {
            self.bump();
        }
    }

    fn disconnect(&self, seq: u64) {
        let now = now_ms();
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.detach(seq, now);
            inner.prune(now);
        }
        self.bump();
    }
}

impl Inner {
    /// Put a connection into a record, creating it if new.
    fn attach(
        &mut self,
        key: Key,
        conn: Conn,
        agent: Option<String>,
        label: Option<String>,
        created_ms: Option<u64>,
        now: u64,
    ) {
        let address = conn.address.clone();
        match self.records.get_mut(&key) {
            Some(record) => {
                // Whether joining this row is evidence of anything.
                //
                // For a credential, yes: the bridge checked it, so a new
                // address really is the same browser having moved, and the
                // history is worth rewriting.
                //
                // For a self-reported id, **no**, and this is the hole the
                // provenance-in-the-key fix did not close. Anyone holding the
                // pairing token can send `?c=<an id someone else is using>`.
                // Rewriting the row from the newcomer would then move the
                // displayed address to the attacker's, overwrite the label and
                // the agent, and — worst of the three — push the victim's real
                // address into `previous_addresses`, rendering "moved from" for
                // a roam that never happened. That is the panel manufacturing
                // evidence, on weaker grounds than `maybe_same_as`, which is
                // scrupulously kept out of the arithmetic.
                //
                // So an unverified join is recorded as a socket and nothing
                // else: the incumbent's details stand.
                let trusted = matches!(record.key.provenance(), Some(Provenance::Credential));

                if trusted {
                    // Only when nothing else is attached. "Moved from" is a
                    // claim about a *reconnect* — this browser was there, is
                    // now here. A second socket opening alongside a live one is
                    // a second tab, and rendering that as a move would invent a
                    // journey out of two things that are both true at once.
                    if record.open.is_empty() && record.address != address {
                        let previous = std::mem::replace(&mut record.address, address);
                        if !record.previous_addresses.contains(&previous) {
                            record.previous_addresses.push(previous);
                            // Bounded: a phone on bad Wi-Fi can rack these up,
                            // and the useful fact is "it moved", not every hop.
                            if record.previous_addresses.len() > 4 {
                                record.previous_addresses.remove(0);
                            }
                        }
                    }
                    if agent.is_some() {
                        record.agent = agent;
                    }
                    if label.is_some() {
                        record.label = label;
                    }
                } else if record.open.is_empty() && record.address != address {
                    // A self-reported row with nothing currently attached: this
                    // is a plain reconnect rather than a second socket, so the
                    // address may move. Still no `previous_addresses` entry —
                    // "it moved" is a claim about one device's history, and one
                    // device is exactly what an unverified id does not
                    // establish.
                    record.address = address;
                }
                if created_ms.is_some() {
                    record.created_ms = created_ms;
                }
                // A revoked device just connected again, with a credential the
                // store still verifies.
                //
                // That is not possible when both halves of revocation ran: the
                // record is deleted first, so `identity_for` could not have
                // resolved this cookie. Reaching here means the credential was
                // never deleted — the half-wired revoke both branches spent a
                // round warning each other about, arriving as a live socket on
                // a row the panel is calling "revoked".
                //
                // Detected rather than assumed away, because the alternative is
                // the worst version of this family: a row that reads *revoked*
                // with a phone attached to it, driving output.
                if record.revoked_at_ms.is_some()
                    && matches!(
                        record.key.provenance(),
                        Some(Provenance::Credential)
                    )
                {
                    record.revoke_contested = true;
                    crate::log_warn!(
                        "[clients] a revoked device reconnected with a credential that still \
                         verifies — the credential was not deleted, so the revoke did not take"
                    );
                }
                record.connected_at_ms = conn.connected_at_ms;
                record.disconnected_at_ms = None;
                record.connections += 1;
                record.open.push(conn);
            }
            None => {
                self.records.insert(
                    key.clone(),
                    Record {
                        key,
                        label,
                        agent,
                        created_ms,
                        revoked_at_ms: None,
                        revoke_contested: false,
                        first_seen_ms: now,
                        connected_at_ms: conn.connected_at_ms,
                        disconnected_at_ms: None,
                        last_heard_ms: now,
                        connections: 1,
                        address,
                        previous_addresses: Vec::new(),
                        open: vec![conn],
                    },
                );
            }
        }
    }

    /// Apply a client's `hello` to the connection it arrived on.
    ///
    /// **Once per connection.** `placement`'s contract has always said a
    /// connection moves records exactly once, but nothing enforced it: every
    /// `hello` reached here and every one called `bump()`, so a client could
    /// drive the desktop panel's repaint rate by sending them in a loop. It
    /// could also hop between identities mid-connection, which is not a thing
    /// any honest client does and not a history worth rendering.
    ///
    /// Returning `false` for the second and later ones makes the contract true
    /// rather than merely documented.
    fn identify(&mut self, seq: u64, claim: Claim, now: u64) -> bool {
        if !self.identified.insert(seq) {
            return false;
        }
        let Some(current) = self.placement.get(&seq).cloned() else {
            return false;
        };
        // A verified credential is not up for renegotiation. Letting a `hello`
        // move a credentialed connection to a self-reported id would be a
        // downgrade the client chooses, and letting it rewrite the label would
        // let a client overwrite the name the *user* gave the device.
        if current.provenance() == Some(Provenance::Credential) {
            return false;
        }
        let target = match &claim.id {
            // A self-reported id, whatever this connection was before. It can
            // never land on a credentialed row: the provenance is part of the
            // key, so a client cannot claim its way into someone else's
            // identity.
            Some(id) => Key::Identified {
                provenance: Provenance::SelfReported,
                id: id.clone(),
            },
            // A hello carrying only a label renames the row it is already on.
            None => {
                if let (Some(record), Some(label)) = (self.records.get_mut(&current), claim.label) {
                    record.label = Some(label);
                    return true;
                }
                return false;
            }
        };
        if target == current {
            if let (Some(record), Some(label)) = (self.records.get_mut(&current), claim.label) {
                record.label = Some(label);
                return true;
            }
            return false;
        }

        // Lift the connection out of where it is and put it on the claimed
        // identity. `connections` is decremented on the way out so a
        // late-arriving hello does not look like an extra connection.
        let Some(from) = self.records.get_mut(&current) else {
            return false;
        };
        let Some(position) = from.open.iter().position(|c| c.seq == seq) else {
            return false;
        };
        let conn = from.open.remove(position);
        let agent = from.agent.clone();
        from.connections = from.connections.saturating_sub(1);
        // An identity a connection merely passed through is not a client that
        // was ever here. Drop the row rather than leaving a phantom beside the
        // real one.
        //
        // This used to apply only to anonymous rows, which covered the common
        // case — a socket that connects unidentified and then says who it is —
        // and missed the shape next to it: arriving as `?c=A` and then sending
        // `hello{clientId: B}` left an "A" row that nothing was ever attached
        // to. The condition that matters is "nothing is left here", not which
        // kind of key it was, and keying it on the kind was an oversight
        // wearing a decision's clothes.
        if from.open.is_empty() && from.connections == 0 {
            self.records.remove(&current);
        } else if from.open.is_empty() {
            from.disconnected_at_ms = Some(now);
        }

        self.attach(target.clone(), conn, agent, claim.label, None, now);
        self.placement.insert(seq, target);
        true
    }

    fn detach(&mut self, seq: u64, now: u64) {
        self.identified.remove(&seq);
        let Some(key) = self.placement.remove(&seq) else {
            return;
        };
        let Some(record) = self.records.get_mut(&key) else {
            return;
        };
        if let Some(position) = record.open.iter().position(|c| c.seq == seq) {
            let conn = record.open.remove(position);
            record.last_heard_ms = record
                .last_heard_ms
                .max(conn.last_heard_ms.load(Ordering::Relaxed));
        }
        if record.open.is_empty() {
            record.disconnected_at_ms = Some(now);
        }
    }

    /// Keep the gone rows bounded, in count and in age.
    ///
    /// **Verified rows are evicted last, and that is a security property rather
    /// than a nicety.** The bound is fixed and the ids that fill it are chosen
    /// by whoever connects, so ordering purely by recency makes retention an
    /// eraser: a client holding the pairing token connects and drops under a
    /// dozen invented ids and the record of a real device's disconnect falls
    /// off the end. That record is the evidence someone opened the panel to
    /// find, and losing it looks like nothing happening rather than like an
    /// attack.
    ///
    /// Age still expires everything equally. This only decides who goes first
    /// when the list is full.
    fn prune(&mut self, now: u64) {
        let mut gone: Vec<(bool, u64, Key)> = self
            .records
            .values()
            .filter_map(|r| {
                r.disconnected_at_ms.map(|at| {
                    (
                        r.key.provenance() == Some(Provenance::Credential),
                        at,
                        r.key.clone(),
                    )
                })
            })
            .collect();
        // Unverified first, then oldest first: the eviction order.
        gone.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        let excess = gone.len().saturating_sub(RETAIN_DISCONNECTED);
        for (index, (_, at, key)) in gone.into_iter().enumerate() {
            if index < excess || now.saturating_sub(at) > RETAIN_DISCONNECTED_MS {
                self.records.remove(&key);
            }
        }
    }
}

impl Record {
    fn render(&self, inner: &Inner, now: u64) -> ClientView {
        let last_heard_ms = self
            .open
            .iter()
            .map(|c| c.last_heard_ms.load(Ordering::Relaxed))
            .max()
            .unwrap_or(self.last_heard_ms)
            .max(self.last_heard_ms);

        ClientView {
            key: self.key.render(),
            identified: matches!(self.key, Key::Identified { .. }),
            provenance: self.key.provenance(),
            id: match &self.key {
                Key::Identified { id, .. } => Some(id.clone()),
                Key::Anonymous(_) => None,
            },
            created_ms: self.created_ms,
            impersonating: self.impersonates_a_credential(inner),
            // A contested row is revocable again: the credential demonstrably
            // still works, so there is something left to delete and retrying is
            // the right action to offer.
            revocable: self.key.provenance() == Some(Provenance::Credential)
                && (self.revoked_at_ms.is_none() || self.revoke_contested),
            revoked_at_ms: self.revoked_at_ms,
            revoke_contested: self.revoke_contested,
            label: self.label.clone(),
            connected: !self.open.is_empty(),
            sockets: self.open.len(),
            connections: self.connections,
            first_seen_ms: self.first_seen_ms,
            connected_at_ms: self.connected_at_ms,
            disconnected_at_ms: self.disconnected_at_ms,
            last_heard_ms,
            address: self.address.clone(),
            previous_addresses: self.previous_addresses.clone(),
            agent: self.agent.clone(),
            maybe_same_as: self.rejoin_hint(inner, now),
        }
    }

    /// Whether this row volunteers an id that a verified credential also holds.
    ///
    /// Cheap because both sets are tiny — this is a handful of rows, not a
    /// directory.
    fn impersonates_a_credential(&self, inner: &Inner) -> bool {
        let Key::Identified {
            provenance: Provenance::SelfReported,
            id,
        } = &self.key
        else {
            return false;
        };
        inner.records.contains_key(&Key::Identified {
            provenance: Provenance::Credential,
            id: id.clone(),
        })
    }

    /// A gone, unidentified row from the same address and agent, close enough
    /// in time to be worth mentioning. Never used for counting.
    fn rejoin_hint(&self, inner: &Inner, now: u64) -> Option<String> {
        if self.open.is_empty() || matches!(self.key, Key::Identified { .. }) {
            return None;
        }
        inner
            .records
            .values()
            .filter(|other| other.key != self.key && other.open.is_empty())
            .filter(|other| other.address_matches(&self.address) && other.agent == self.agent)
            .filter(|other| {
                other
                    .disconnected_at_ms
                    .is_some_and(|at| now.saturating_sub(at) <= REJOIN_HINT_MS)
            })
            .max_by_key(|other| other.disconnected_at_ms.unwrap_or(0))
            .map(|other| other.key.render())
    }

    /// Same host, whatever ephemeral port it used this time.
    fn address_matches(&self, other: &str) -> bool {
        fn host(addr: &str) -> &str {
            addr.rsplit_once(':').map(|(host, _)| host).unwrap_or(addr)
        }
        host(&self.address) == host(other)
    }
}

/// A registered connection. Dropping it deregisters.
pub struct ClientHandle {
    registry: Arc<ClientRegistry>,
    seq: u64,
    last_heard: Arc<AtomicU64>,
    closed: watch::Receiver<bool>,
}

impl ClientHandle {
    /// Resolves when this connection has been revoked and must close.
    ///
    /// The relay selects on it and sends a close frame that says why. A
    /// revoked device that keeps its socket until the network happens to drop
    /// it is not revoked, and one that closes silently is indistinguishable
    /// from a dead router — the ambiguity this project has spent a day
    /// removing.
    pub async fn revoked(&mut self) {
        // `changed()` only resolves on a *change*, so a socket registered after
        // the revoke would wait forever. Check the current value first.
        if *self.closed.borrow_and_update() {
            return;
        }
        while self.closed.changed().await.is_ok() {
            if *self.closed.borrow_and_update() {
                return;
            }
        }
        // The sender is gone, which happens only when this connection has been
        // deregistered. Never resolve — the relay is already shutting down.
        std::future::pending::<()>().await
    }

    /// Apply a claim the client sent after connecting.
    pub fn identify(&self, claim: Claim) {
        if claim.is_empty() {
            return;
        }
        self.registry.identify(self.seq, claim);
    }

    /// Note that a frame arrived from this client.
    ///
    /// Call this **only for inbound traffic** — a Pong, a command, anything the
    /// peer actually sent. Calling it after a successful write would make the
    /// field describe our own kernel rather than the client, which is what it
    /// used to do and why it could not detect a phone that had walked away.
    ///
    /// Lock-free and silent: a live client refreshes this about once a second
    /// via Pong, so it must not wake the UI.
    pub fn heard(&self) {
        self.last_heard.store(now_ms(), Ordering::Relaxed);
    }
}

impl Drop for ClientHandle {
    fn drop(&mut self) {
        self.registry.disconnect(self.seq);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> Arc<ClientRegistry> {
        Arc::new(ClientRegistry::new())
    }

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    /// A self-reported id — what a client can offer before it has a credential.
    fn claimed(id: &str) -> ConnectingIdentity {
        ConnectingIdentity::Claimed(id.to_string())
    }

    /// A verified per-device credential, as the pairing flow would hand it over.
    fn credential(id: &str, label: Option<&str>) -> ConnectingIdentity {
        ConnectingIdentity::Credential(Credential {
            id: id.to_string(),
            label: label.map(str::to_string),
            created_ms: Some(1_000),
        })
    }

    /// A registry that verifies exactly the credentials named, as `bridge-tls`
    /// will once its store exists.
    fn registry_with_credentials(
        known: &'static [(&'static str, &'static str)],
    ) -> Arc<ClientRegistry> {
        let reg = registry();
        reg.set_credential_resolver(Arc::new(move |cookie: Option<&str>| {
            let cookie = cookie?;
            known.iter().find_map(|(needle, id)| {
                cookie.contains(needle).then(|| Credential {
                    id: (*id).to_string(),
                    label: None,
                    created_ms: Some(1_000),
                })
            })
        }));
        reg
    }

    const PHONE: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_4 like Mac OS X) \
         AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Mobile/15E148 Safari/604.1";

    #[test]
    fn an_empty_bridge_reports_nothing_rather_than_guessing() {
        let view = registry().view();
        assert_eq!(view.connections, 0);
        assert_eq!(view.browsers, BrowserCount::Reported { count: 0, provenance: Provenance::Credential });
        assert!(view.clients.is_empty());
        assert!(!view.any_unidentified);
    }

    /// The whole point. One client that drops and comes back is one client,
    /// and the panel must say one — not two.
    #[test]
    fn a_reconnect_with_an_id_is_one_client_not_two() {
        let reg = registry();
        let id = "phone-9f2c41ab".to_string();

        let first = reg.connect(addr("192.168.0.31:51002"), Some(PHONE), claimed(&id));
        assert_eq!(reg.view().connections, 1);
        drop(first);

        // Roamed to a different access point: new port, new IP, same device.
        let _second = reg.connect(addr("192.168.0.77:51999"), Some(PHONE), claimed(&id));

        let view = reg.view();
        assert_eq!(view.connections, 1);
        assert_eq!(view.browsers, BrowserCount::Reported { count: 1, provenance: Provenance::SelfReported });
        assert_eq!(view.clients.len(), 1, "a roam is not a second device");

        let client = &view.clients[0];
        assert!(client.connected);
        assert_eq!(client.connections, 2, "the reconnect is stated, not hidden");
        assert_eq!(client.address, "192.168.0.77:51999");
        // No "moved from" line. An unverified id groups the sockets, which is
        // useful, but it does not establish that one device did the moving —
        // and a roam is a claim about one device's history. See
        // `an_unverified_join_cannot_rewrite_the_row_it_joins`.
        assert!(client.previous_addresses.is_empty());
    }

    /// The same roam, with a credential behind it: now the history is real and
    /// the panel says so.
    #[test]
    fn a_verified_roam_is_recorded_as_an_address_change() {
        let reg = registry();
        let first = reg.connect(addr("192.168.0.31:51002"), Some(PHONE), credential("dev-0001", None));
        drop(first);
        let _second = reg.connect(addr("192.168.0.77:51999"), Some(PHONE), credential("dev-0001", None));

        let view = reg.view();
        assert_eq!(view.clients.len(), 1);
        assert_eq!(view.clients[0].address, "192.168.0.77:51999");
        assert_eq!(
            view.clients[0].previous_addresses,
            vec!["192.168.0.31:51002".to_string()],
            "the bridge checked the credential, so the roam is evidence"
        );
    }

    /// The impersonation that survived folding provenance into the key.
    ///
    /// Anyone holding the pairing token can send a `?c=` or a `hello` naming an
    /// id someone else is using. Grouping the sockets is a tolerable
    /// consequence of an unverified id — but the newcomer must not be able to
    /// **rewrite the row it joined**: not the displayed address, not the label,
    /// not the agent, and above all not push the incumbent's address into
    /// `previous_addresses`, which would render a "moved from" line for a roam
    /// that never happened. Manufacturing evidence is worse than showing none.
    #[test]
    fn an_unverified_join_cannot_rewrite_the_row_it_joins() {
        const ATTACKER_UA: &str = "Mozilla/5.0 (Windows NT 10.0) Firefox/121.0";
        let reg = registry();

        let _victim = reg.connect(addr("192.168.0.31:51002"), Some(PHONE), claimed("victim-phone"));
        let attacker = reg.connect(addr("10.9.9.9:40000"), Some(ATTACKER_UA), ConnectingIdentity::Unidentified);
        attacker.identify(Claim {
            id: Some("victim-phone".into()),
            label: Some("Definitely the phone".into()),
        });

        let view = reg.view();
        let row = view
            .clients
            .iter()
            .find(|c| c.id.as_deref() == Some("victim-phone"))
            .expect("the shared row");

        assert_eq!(row.address, "192.168.0.31:51002", "the incumbent's address stands");
        assert!(
            row.previous_addresses.is_empty(),
            "no fabricated roam: the victim's address must not become history"
        );
        assert_eq!(row.label, None, "the attacker cannot name someone else's row");
        assert_eq!(
            row.agent.as_deref(),
            agent_hint(PHONE).as_deref(),
            "the incumbent's agent stands"
        );
        // And the count says these are self-reported, so nothing downstream
        // reads two sockets on one volunteered id as a verified fact.
        assert_eq!(
            view.browsers,
            BrowserCount::Reported {
                count: 1,
                provenance: Provenance::SelfReported
            }
        );
    }

    /// One weak member makes the whole count weak. A mixed view must not
    /// inherit the credentialed wording.
    #[test]
    fn the_weakest_provenance_present_governs_the_count() {
        let reg = registry();
        let _verified = reg.connect(addr("192.168.0.31:5000"), Some(PHONE), credential("dev-0001", None));
        assert_eq!(
            reg.view().browsers,
            BrowserCount::Reported {
                count: 1,
                provenance: Provenance::Credential
            }
        );

        let _volunteered = reg.connect(addr("192.168.0.32:5000"), Some(PHONE), claimed("phone-aaaaaaaa"));
        assert_eq!(
            reg.view().browsers,
            BrowserCount::Reported {
                count: 2,
                provenance: Provenance::SelfReported
            },
            "a count is only as trustworthy as its softest member"
        );
    }

    #[test]
    fn two_identified_browsers_are_two() {
        let reg = registry();
        let _phone = reg.connect(
            addr("192.168.0.31:5000"),
            Some(PHONE),
            claimed("phone-aaaaaaaa"),
        );
        let _tablet = reg.connect(
            addr("192.168.0.32:5000"),
            Some(PHONE),
            claimed("tablet-bbbbbbbb"),
        );

        let view = reg.view();
        assert_eq!(view.connections, 2);
        assert_eq!(view.browsers, BrowserCount::Reported { count: 2, provenance: Provenance::SelfReported });
    }

    /// The honest case, and today's default: nothing claimed an id, so the
    /// count is a floor with a stated reason.
    #[test]
    fn unidentified_clients_produce_a_floor_and_not_a_count() {
        let reg = registry();
        let _a = reg.connect(
            addr("192.168.0.31:5000"),
            Some(PHONE),
            ConnectingIdentity::Unidentified,
        );
        let _b = reg.connect(
            addr("192.168.0.31:5001"),
            Some(PHONE),
            ConnectingIdentity::Unidentified,
        );

        let view = reg.view();
        assert_eq!(view.connections, 2, "sockets are known exactly");
        assert_eq!(
            view.browsers,
            BrowserCount::AtLeast {
                count: 0,
                unidentified: 2
            },
            "two sockets from one address could be one device or two"
        );
        assert!(view.any_unidentified);
    }

    /// One identified client plus one anonymous one is not "two devices".
    #[test]
    fn a_single_unidentified_client_makes_the_whole_count_a_floor() {
        let reg = registry();
        let _known = reg.connect(
            addr("192.168.0.31:5000"),
            Some(PHONE),
            claimed("phone-aaaaaaaa"),
        );
        let _unknown = reg.connect(
            addr("192.168.0.99:5000"),
            None,
            ConnectingIdentity::Unidentified,
        );

        assert_eq!(
            reg.view().browsers,
            BrowserCount::AtLeast {
                count: 1,
                unidentified: 1
            }
        );
    }

    /// Anonymous connections must never merge, however similar they look.
    /// Merging them is the confidently-wrong number this module exists to
    /// avoid.
    #[test]
    fn identical_looking_anonymous_connections_stay_separate() {
        let reg = registry();
        let _a = reg.connect(
            addr("192.168.0.31:5000"),
            Some(PHONE),
            ConnectingIdentity::Unidentified,
        );
        let _b = reg.connect(
            addr("192.168.0.31:5001"),
            Some(PHONE),
            ConnectingIdentity::Unidentified,
        );
        let view = reg.view();
        assert_eq!(view.clients.len(), 2);
        assert!(view.clients.iter().all(|c| !c.identified));
    }

    /// A hello that arrives after the socket opened moves the connection onto
    /// its identity and leaves no phantom behind.
    #[test]
    fn a_late_hello_claims_the_connection_without_leaving_a_ghost() {
        let reg = registry();
        let handle = reg.connect(
            addr("192.168.0.31:5000"),
            Some(PHONE),
            ConnectingIdentity::Unidentified,
        );
        assert!(reg.view().any_unidentified);

        handle.identify(Claim {
            id: Some("phone-aaaaaaaa".into()),
            label: Some("Sara's phone".into()),
        });

        let view = reg.view();
        assert_eq!(view.connections, 1);
        assert_eq!(view.clients.len(), 1, "no leftover unidentified row");
        assert_eq!(view.browsers, BrowserCount::Reported { count: 1, provenance: Provenance::SelfReported });
        let client = &view.clients[0];
        assert!(client.identified);
        assert_eq!(client.label.as_deref(), Some("Sara's phone"));
        assert_eq!(
            client.connections, 1,
            "identifying a live socket is not a second connection"
        );
    }

    /// A client must not be able to re-identify in a loop. Each `hello` used to
    /// wake the desktop panel, so the repaint rate was the client's to choose.
    #[test]
    fn only_the_first_hello_on_a_connection_counts() {
        let reg = registry();
        let handle = reg.connect(addr("192.168.0.31:5000"), Some(PHONE), ConnectingIdentity::Unidentified);
        handle.identify(Claim {
            id: Some("phone-aaaaaaaa".into()),
            label: Some("First".into()),
        });

        let mut rx = reg.subscribe();
        let _ = rx.borrow_and_update();

        for n in 0..10 {
            handle.identify(Claim {
                id: Some(format!("hop-{n:08}")),
                label: Some(format!("rename {n}")),
            });
        }

        assert!(!rx.has_changed().unwrap(), "no repaints the client can drive");
        let view = reg.view();
        assert_eq!(view.clients.len(), 1, "no identity hopping");
        assert_eq!(view.clients[0].id.as_deref(), Some("phone-aaaaaaaa"));
        assert_eq!(view.clients[0].label.as_deref(), Some("First"));
    }

    /// A reconnect that identifies late still merges with the earlier session.
    #[test]
    fn a_late_hello_merges_with_the_same_id_seen_before() {
        let reg = registry();
        let first = reg.connect(
            addr("192.168.0.31:5000"),
            Some(PHONE),
            claimed("phone-aaaaaaaa"),
        );
        drop(first);

        let second = reg.connect(
            addr("192.168.0.31:5001"),
            Some(PHONE),
            ConnectingIdentity::Unidentified,
        );
        second.identify(Claim {
            id: Some("phone-aaaaaaaa".into()),
            ..Default::default()
        });

        let view = reg.view();
        assert_eq!(view.clients.len(), 1);
        assert_eq!(view.clients[0].connections, 2);
        assert!(view.clients[0].connected);
    }

    #[test]
    fn a_disconnect_leaves_a_row_that_says_it_is_gone() {
        let reg = registry();
        let handle = reg.connect(
            addr("192.168.0.31:5000"),
            Some(PHONE),
            claimed("phone-aaaaaaaa"),
        );
        drop(handle);

        let view = reg.view();
        assert_eq!(view.connections, 0);
        assert_eq!(view.browsers, BrowserCount::Reported { count: 0, provenance: Provenance::Credential });
        assert_eq!(view.clients.len(), 1);
        assert!(!view.clients[0].connected);
        assert!(view.clients[0].disconnected_at_ms.is_some());
    }

    /// Two tabs on one phone are one browser with two sockets, and the panel
    /// says both numbers rather than picking one.
    #[test]
    fn two_sockets_on_one_identity_are_one_browser() {
        let reg = registry();
        let _a = reg.connect(
            addr("192.168.0.31:5000"),
            Some(PHONE),
            claimed("phone-aaaaaaaa"),
        );
        let _b = reg.connect(
            addr("192.168.0.31:5001"),
            Some(PHONE),
            claimed("phone-aaaaaaaa"),
        );

        let view = reg.view();
        assert_eq!(view.connections, 2);
        assert_eq!(view.browsers, BrowserCount::Reported { count: 1, provenance: Provenance::SelfReported });
        assert_eq!(view.clients[0].sockets, 2);
    }

    /// The guess is offered as a guess, and never folded into the numbers.
    #[test]
    fn a_plausible_rejoin_is_hinted_but_never_counted() {
        let reg = registry();
        let first = reg.connect(
            addr("192.168.0.31:5000"),
            Some(PHONE),
            ConnectingIdentity::Unidentified,
        );
        let first_key = reg.view().clients[0].key.clone();
        drop(first);
        let _second = reg.connect(
            addr("192.168.0.31:5010"),
            Some(PHONE),
            ConnectingIdentity::Unidentified,
        );

        let view = reg.view();
        let live = view.clients.iter().find(|c| c.connected).unwrap();
        assert_eq!(live.maybe_same_as.as_deref(), Some(first_key.as_str()));
        assert_eq!(
            view.browsers,
            BrowserCount::AtLeast {
                count: 0,
                unidentified: 1
            },
            "a hint must not become a count"
        );
    }

    #[test]
    fn a_dropped_handle_deregisters_even_on_an_early_return() {
        let reg = registry();
        {
            let _handle = reg.connect(
                addr("127.0.0.1:5000"),
                None,
                ConnectingIdentity::Unidentified,
            );
            assert_eq!(reg.view().connections, 1);
        }
        assert_eq!(reg.view().connections, 0);
    }

    #[test]
    fn the_version_channel_wakes_on_a_connect_and_a_disconnect() {
        let reg = registry();
        let mut rx = reg.subscribe();
        assert!(!rx.has_changed().unwrap());

        let handle = reg.connect(
            addr("127.0.0.1:5000"),
            None,
            ConnectingIdentity::Unidentified,
        );
        assert!(rx.has_changed().unwrap());
        let _ = rx.borrow_and_update();

        drop(handle);
        assert!(rx.has_changed().unwrap());
    }

    /// `heard` runs once a second per client. If it woke the watch, the panel
    /// would repaint forever.
    #[test]
    fn being_heard_from_does_not_wake_the_watch() {
        let reg = registry();
        let handle = reg.connect(
            addr("127.0.0.1:5000"),
            None,
            ConnectingIdentity::Unidentified,
        );
        let mut rx = reg.subscribe();
        let _ = rx.borrow_and_update();
        handle.heard();
        assert!(!rx.has_changed().unwrap());
    }

    // -----------------------------------------------------------------------
    // Per-device credentials
    // -----------------------------------------------------------------------

    /// Without a resolver installed there are no credentials on this build, and
    /// the view says which world it is in rather than leaving an absence to be
    /// read as "nobody has one".
    #[test]
    fn a_bridge_without_pairing_says_credentials_are_unavailable() {
        assert!(!registry().view().credentials_available);
        let reg = registry_with_credentials(&[]);
        assert!(reg.view().credentials_available);
    }

    /// The one place identity is decided. A verified credential beats a
    /// self-reported id on the same connection, because one of them was checked.
    #[test]
    fn a_credential_outranks_an_id_the_client_chose() {
        let reg = registry_with_credentials(&[("good-secret", "dev-0001")]);

        let verified = reg.identity_for(
            "/ws?t=x&c=phone-aaaaaaaa",
            Some("coyote_device=good-secret"),
        );
        assert!(matches!(
            verified,
            ConnectingIdentity::Credential(Credential { ref id, .. }) if id == "dev-0001"
        ));

        // No cookie, so we fall back to what the client says about itself.
        let claimed_only = reg.identity_for("/ws?t=x&c=phone-aaaaaaaa", None);
        assert_eq!(
            claimed_only,
            ConnectingIdentity::Claimed("phone-aaaaaaaa".into())
        );

        // A cookie the store does not recognise is not an identity.
        let unknown = reg.identity_for("/ws?t=x", Some("coyote_device=forged"));
        assert_eq!(unknown, ConnectingIdentity::Unidentified);
    }

    /// The credentialed row carries where its identity came from, so the panel
    /// can say "verified" rather than treating all identity as equal.
    #[test]
    fn a_credentialed_row_is_marked_as_verified_and_revocable() {
        let reg = registry();
        let _c = reg.connect(
            addr("192.168.0.31:5000"),
            Some(PHONE),
            credential("dev-0001", Some("Sara's phone")),
        );
        let _s = reg.connect(
            addr("192.168.0.32:5000"),
            Some(PHONE),
            claimed("phone-aaaaaaaa"),
        );

        let view = reg.view();
        let verified = view
            .clients
            .iter()
            .find(|c| c.id.as_deref() == Some("dev-0001"))
            .unwrap();
        assert_eq!(verified.provenance, Some(Provenance::Credential));
        assert!(verified.revocable);
        assert_eq!(verified.label.as_deref(), Some("Sara's phone"));
        assert_eq!(verified.created_ms, Some(1_000));

        let self_reported = view
            .clients
            .iter()
            .find(|c| c.id.as_deref() == Some("phone-aaaaaaaa"))
            .unwrap();
        assert_eq!(self_reported.provenance, Some(Provenance::SelfReported));
        assert!(
            !self_reported.revocable,
            "revoking a self-reported id closes a socket that reconnects instantly"
        );
    }

    /// The attack a weaker key would allow: anyone holding the pairing token
    /// sends `?c=<someone's credential id>` and appears as their phone.
    #[test]
    fn a_self_reported_id_cannot_impersonate_a_credential() {
        let reg = registry();
        let _real = reg.connect(
            addr("192.168.0.31:5000"),
            Some(PHONE),
            credential("dev-0001", None),
        );
        let _forger = reg.connect(addr("10.0.0.9:5000"), Some(PHONE), claimed("dev-0001"));

        let view = reg.view();
        assert_eq!(view.clients.len(), 2, "the two must not merge");
        assert_eq!(view.browsers, BrowserCount::Reported { count: 2, provenance: Provenance::SelfReported });
    }

    /// Nor by sending a `hello` after the fact.
    #[test]
    fn a_hello_cannot_downgrade_or_rename_a_credentialed_connection() {
        let reg = registry();
        let handle = reg.connect(
            addr("192.168.0.31:5000"),
            Some(PHONE),
            credential("dev-0001", Some("Sara's phone")),
        );
        handle.identify(Claim {
            id: Some("phone-aaaaaaaa".into()),
            label: Some("Not Sara's phone".into()),
        });

        let view = reg.view();
        assert_eq!(view.clients.len(), 1);
        assert_eq!(view.clients[0].id.as_deref(), Some("dev-0001"));
        assert_eq!(
            view.clients[0].label.as_deref(),
            Some("Sara's phone"),
            "the user's own label is not the client's to overwrite"
        );
    }

    /// The requirement revocation lives or dies on: the *live* socket closes.
    /// A revoked phone that keeps driving output until it happens to drop is
    /// not revoked.
    #[tokio::test]
    async fn revoking_a_credential_closes_its_live_sockets() {
        let reg = registry();
        let mut first = reg.connect(
            addr("192.168.0.31:5000"),
            Some(PHONE),
            credential("dev-0001", None),
        );
        let mut second = reg.connect(
            addr("192.168.0.31:5001"),
            Some(PHONE),
            credential("dev-0001", None),
        );
        let mut other = reg.connect(
            addr("192.168.0.40:5000"),
            Some(PHONE),
            credential("dev-0002", None),
        );

        assert_eq!(reg.revoke("dev-0001"), 2, "both of that device's sockets");

        // Both wake immediately.
        for handle in [&mut first, &mut second] {
            tokio::time::timeout(std::time::Duration::from_secs(1), handle.revoked())
                .await
                .expect("a revoked socket must be told at once");
        }
        // The bystander is untouched — revoking one device is not a kick-all.
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), other.revoked())
                .await
                .is_err()
        );
    }

    /// The panel is what someone looks at immediately after pressing revoke,
    /// and the row outlives the socket by minutes. It must not go on saying
    /// "verified" for a credential that has just been deleted.
    #[test]
    fn a_revoked_row_says_so_rather_than_staying_verified() {
        let reg = registry();
        let handle = reg.connect(
            addr("192.168.0.31:5000"),
            Some(PHONE),
            credential("dev-0001", Some("Sara's phone")),
        );
        assert!(reg.view().clients[0].revocable);

        reg.revoke("dev-0001");
        drop(handle); // the relay notices and the socket closes

        let view = reg.view();
        assert_eq!(view.connections, 0);
        let row = &view.clients[0];
        assert!(row.revoked_at_ms.is_some(), "the row records what happened");
        assert!(
            !row.revocable,
            "there is nothing left to revoke, so the action must not be offered again"
        );
        // The provenance stays truthful about what the connection *was* — the
        // row is annotated, not rewritten.
        assert_eq!(row.provenance, Some(Provenance::Credential));
    }

    /// The detector for a half-wired revoke: the socket was closed but the
    /// credential was never deleted, so the device comes straight back.
    ///
    /// Without this the panel would show a row tagged "revoked" with a live
    /// socket on it — reassuring, on the confirmation screen, about a phone
    /// that is at that moment connected.
    #[test]
    fn a_revoked_device_that_reconnects_contests_the_revoke() {
        let reg = registry();
        let first = reg.connect(addr("192.168.0.31:5000"), Some(PHONE), credential("dev-0001", None));
        reg.revoke("dev-0001");
        drop(first);
        assert!(!reg.view().clients[0].revoke_contested);

        // Only reachable when the credential still verifies — i.e. the store
        // half of revocation never ran.
        let _again = reg.connect(addr("192.168.0.31:5001"), Some(PHONE), credential("dev-0001", None));

        let view = reg.view();
        assert_eq!(view.clients.len(), 1);
        let row = &view.clients[0];
        assert!(row.connected, "the device is back");
        assert!(row.revoke_contested, "and the panel must not call that revoked");
        assert!(
            row.revocable,
            "the credential still works, so retrying is a real action"
        );
    }

    /// A revoked device that comes back *without* a credential is an ordinary
    /// unidentified client, not a contested revoke. The store did its job; the
    /// browser is simply back on the pairing page.
    #[test]
    fn a_revoked_device_returning_uncredentialed_is_not_contested() {
        let reg = registry();
        let first = reg.connect(addr("192.168.0.31:5000"), Some(PHONE), credential("dev-0001", None));
        reg.revoke("dev-0001");
        drop(first);

        let _stranger = reg.connect(
            addr("192.168.0.31:5001"),
            Some(PHONE),
            ConnectingIdentity::Unidentified,
        );

        let view = reg.view();
        let revoked = view.clients.iter().find(|c| c.id.as_deref() == Some("dev-0001")).unwrap();
        assert!(!revoked.revoke_contested);
        assert!(!revoked.connected);
        assert_eq!(view.connections, 1, "the new socket is its own row");
    }

    /// Revoking a device with no live socket still changes what the panel says.
    /// The user pressed the button; something has to answer.
    #[test]
    fn revoking_an_offline_device_still_marks_the_row_and_wakes_the_panel() {
        let reg = registry();
        let handle = reg.connect(addr("192.168.0.31:5000"), Some(PHONE), credential("dev-0001", None));
        drop(handle);

        let mut rx = reg.subscribe();
        let _ = rx.borrow_and_update();

        assert_eq!(reg.revoke("dev-0001"), 0, "nothing was open");
        assert!(rx.has_changed().unwrap(), "the row's meaning changed");
        assert!(reg.view().clients[0].revoked_at_ms.is_some());
    }

    /// Revocation is keyed on a verified credential. A self-reported id is not
    /// revocable, and pretending otherwise would ship a button that appears to
    /// work.
    #[test]
    fn a_self_reported_id_cannot_be_revoked() {
        let reg = registry();
        let _handle = reg.connect(
            addr("192.168.0.31:5000"),
            Some(PHONE),
            claimed("phone-aaaaaaaa"),
        );
        assert_eq!(reg.revoke("phone-aaaaaaaa"), 0);
        assert_eq!(reg.view().connections, 1);
    }

    // -----------------------------------------------------------------------
    // Structurally unexpected claims
    //
    // Not absent and not forged — *malformed in shape*. Everything above tests
    // a client that either says nothing or says something false. These test a
    // client that says something the code did not have a category for, which is
    // where the last two defects in this module came from.
    // -----------------------------------------------------------------------

    /// The rendered key namespaces cannot be made to collide.
    ///
    /// `:` is a legal character in a self-reported id, so `c:` + `red:x` looked
    /// like it could collide with `cred:` + `x` — and a duplicate `key` would
    /// crash the panel's keyed `{#each}`, reachable by anyone with the token.
    /// It cannot: the prefixes differ at index 1, so no suffix can reconcile
    /// them. Asserted rather than left to that argument, because the argument
    /// is exactly the kind that stops holding when someone renames a prefix.
    #[test]
    fn key_namespaces_cannot_be_forged_into_each_other() {
        let credential = Key::Identified {
            provenance: Provenance::Credential,
            id: "dev-abc123".into(),
        };
        for hostile in [
            "red:dev-abc123",
            "c:dev-abc123",
            "cred:dev-abc123",
            "anon:0",
            "::::::::",
        ] {
            let claimed = Key::Identified {
                provenance: Provenance::SelfReported,
                id: hostile.to_string(),
            };
            assert_ne!(claimed.render(), credential.render(), "{hostile}");
            assert_ne!(claimed.render(), Key::Anonymous(0).render(), "{hostile}");
        }
    }

    /// A self-reported id that matches a real credential is not merely
    /// separate — it is a claim worth showing as one.
    ///
    /// Keying on provenance already stops the merge, so the counts are right.
    /// But both rows then render the same id string, and the panel is what
    /// someone reads *before* revoking. Two identical-looking ids with no
    /// signal between them is how the wrong row gets picked.
    #[test]
    fn a_self_reported_id_matching_a_credential_is_flagged_as_impersonation() {
        let reg = registry();
        let _real = reg.connect(addr("192.168.0.31:5000"), Some(PHONE), credential("dev-abc123", None));
        let _fake = reg.connect(addr("10.9.9.9:40000"), Some(PHONE), claimed("dev-abc123"));

        let view = reg.view();
        assert_eq!(view.clients.len(), 2, "still not merged");

        let real = view
            .clients
            .iter()
            .find(|c| c.provenance == Some(Provenance::Credential))
            .unwrap();
        let fake = view
            .clients
            .iter()
            .find(|c| c.provenance == Some(Provenance::SelfReported))
            .unwrap();
        assert!(
            fake.impersonating,
            "a volunteered id matching a verified one must say so"
        );
        assert!(!real.impersonating, "the genuine row is not the suspect one");
    }

    /// An identity a connection passes *through* must not be left behind.
    ///
    /// Arriving as `?c=A` and then sending `hello{clientId: B}` moves the
    /// socket, and the vacated row used to persist as a "recently
    /// disconnected" entry for an identity nothing was ever really attached to.
    /// Anonymous rows were already cleaned up this way; self-reported ones were
    /// not, and the difference was an oversight rather than a decision.
    #[test]
    fn an_identity_a_connection_passed_through_leaves_no_row() {
        let reg = registry();
        let handle = reg.connect(addr("192.168.0.31:5000"), Some(PHONE), claimed("first-aaaaaaaa"));
        handle.identify(Claim {
            id: Some("second-bbbbbbbb".into()),
            label: None,
        });

        let view = reg.view();
        assert_eq!(view.clients.len(), 1, "no ghost for the id it passed through");
        assert_eq!(view.clients[0].id.as_deref(), Some("second-bbbbbbbb"));
        assert_eq!(view.clients[0].connections, 1);
    }

    /// Retention must not be an attacker's eraser.
    ///
    /// The gone-row list is bounded, and the ids that fill it are chosen by
    /// whoever connects. So a client with the pairing token can churn through
    /// invented ids and evict the record of a real device's disconnect — which
    /// is the evidence someone is looking at the panel to find. Verified rows
    /// are therefore evicted last.
    #[test]
    fn churn_from_unverified_clients_cannot_evict_verified_history() {
        let reg = registry();
        let real = reg.connect(addr("192.168.0.31:5000"), Some(PHONE), credential("dev-real", None));
        drop(real);

        for n in 0..(RETAIN_DISCONNECTED * 3) {
            let noise = reg.connect(
                addr("10.9.9.9:40000"),
                Some(PHONE),
                ConnectingIdentity::Claimed(format!("noise-{n:08}")),
            );
            drop(noise);
        }

        let view = reg.view();
        assert!(
            view.clients.iter().any(|c| c.id.as_deref() == Some("dev-real")),
            "the verified device's disconnect survived the churn"
        );
        assert!(view.clients.len() <= RETAIN_DISCONNECTED);
    }

    // -----------------------------------------------------------------------
    // Revocation, probed for shape
    //
    // The surface where a wrong answer is most expensive: a device the user
    // believes is cut off that is not.
    // -----------------------------------------------------------------------

    /// Revoking a credential whose socket has already closed still marks the
    /// row, and still reports nothing was disconnected. Both facts matter and
    /// they are different facts.
    #[test]
    fn revoking_an_already_closed_socket_marks_without_claiming_a_disconnect() {
        let reg = registry();
        let handle = reg.connect(addr("192.168.0.31:5000"), Some(PHONE), credential("dev-0001", None));
        drop(handle);

        assert_eq!(reg.revoke("dev-0001"), 0, "nothing was open to close");
        let view = reg.view();
        assert!(view.clients[0].revoked_at_ms.is_some());
        assert!(!view.clients[0].revocable, "nothing left to revoke");
    }

    /// Revoking twice is not an error and does not double-count. The second
    /// call finds the row already marked and no sockets left to close.
    #[test]
    fn revoking_twice_is_harmless() {
        let reg = registry();
        let handle = reg.connect(addr("192.168.0.31:5000"), Some(PHONE), credential("dev-0001", None));

        assert_eq!(reg.revoke("dev-0001"), 1);
        assert!(reg.view().clients[0].revoked_at_ms.is_some());

        // The socket is signalled but the relay has not dropped it yet, which
        // is the real interleaving: `revoke` returns before the task wakes.
        assert_eq!(reg.revoke("dev-0001"), 1, "still open, still signalled");

        drop(handle);
        assert_eq!(reg.revoke("dev-0001"), 0, "now there is nothing to close");
        assert!(reg.view().clients[0].revoked_at_ms.is_some());
    }

    /// A revoke that lands while a connection is still being set up must still
    /// reach it. The signal is a *level*, not an edge — a handle created before
    /// the revoke sees it on its first poll rather than waiting for a change
    /// that has already happened.
    #[tokio::test]
    async fn a_revoke_during_setup_is_not_missed() {
        let reg = registry();
        let mut handle = reg.connect(addr("192.168.0.31:5000"), Some(PHONE), credential("dev-0001", None));

        // Revoked before anything ever polls `revoked()`, which is the window
        // between registering and the relay reaching its select.
        reg.revoke("dev-0001");

        tokio::time::timeout(std::time::Duration::from_secs(1), handle.revoked())
            .await
            .expect("a revoke issued before the first poll must still be seen");
    }

    /// Revoking a credential must not touch a different connection that merely
    /// volunteers the same string.
    ///
    /// The impersonating row is the one an attacker controls, so if revocation
    /// followed the id rather than the identity, the attacker would choose
    /// whose socket got closed.
    #[test]
    fn revoking_a_credential_leaves_an_impersonator_alone_and_vice_versa() {
        let reg = registry();
        let mut real = reg.connect(addr("192.168.0.31:5000"), Some(PHONE), credential("dev-abc123", None));
        let _fake = reg.connect(addr("10.9.9.9:40000"), Some(PHONE), claimed("dev-abc123"));

        assert_eq!(reg.revoke("dev-abc123"), 1, "exactly the verified socket");

        let view = reg.view();
        let fake = view
            .clients
            .iter()
            .find(|c| c.provenance == Some(Provenance::SelfReported))
            .unwrap();
        assert!(fake.connected, "the impersonator's socket is untouched");
        assert!(fake.revoked_at_ms.is_none(), "and it is not marked revoked");

        // And the verified one really was the one signalled.
        assert!(
            futures::FutureExt::now_or_never(Box::pin(real.revoked())).is_some(),
            "the verified socket was told"
        );
    }

    /// A credential store that returns an unusable id must not produce a
    /// verified row with no name. Falling back to unidentified is honest; a
    /// blank verified row is not.
    #[test]
    fn an_unusable_credential_id_is_refused_rather_than_rendered() {
        for bad in ["", "   ", "\u{0}bad"] {
            let reg = registry();
            let id = bad.to_string();
            reg.set_credential_resolver(Arc::new(move |_: Option<&str>| {
                Some(Credential {
                    id: id.clone(),
                    label: None,
                    created_ms: None,
                })
            }));
            assert_eq!(
                reg.identity_for("/ws?t=x", Some("coyote_device=whatever")),
                ConnectingIdentity::Unidentified,
                "{bad:?} must not become a verified identity"
            );
        }
    }

    #[test]
    fn ids_are_read_from_the_query_string() {
        assert_eq!(
            id_from_query("/ws?t=abc&c=phone-aaaaaaaa"),
            Some("phone-aaaaaaaa".into())
        );
        assert_eq!(id_from_query("/ws?t=abc"), None);
        // Too short to be a device id, and short ids collide across devices.
        assert_eq!(id_from_query("/ws?c=phone"), None);
        // Structure characters cannot reach a map key or a window.
        assert_eq!(id_from_query("/ws?c=%3Cscript%3Ealert"), None);
        assert_eq!(id_from_query(&format!("/ws?c={}", "a".repeat(65))), None);
    }

    #[test]
    fn labels_are_bounded_and_stripped_of_control_characters() {
        assert_eq!(
            sanitise_label("  Sara's phone \n"),
            Some("Sara's phone".into())
        );
        assert_eq!(sanitise_label("   "), None);
        assert_eq!(sanitise_label("\u{0}\u{7}"), None);
        assert_eq!(
            sanitise_label(&"x".repeat(200)).unwrap().len(),
            MAX_LABEL_LEN
        );
    }

    #[test]
    fn a_hello_is_parsed_and_anything_else_is_left_alone() {
        let claim = parse_hello(r#"{"type":"hello","clientId":"phone-aaaaaaaa","label":"Phone"}"#)
            .expect("a hello");
        assert_eq!(claim.id.as_deref(), Some("phone-aaaaaaaa"));
        assert_eq!(claim.label.as_deref(), Some("Phone"));

        // A command must fall through to the command parser untouched.
        assert!(parse_hello(r#"{"type":"seek","positionS":12.0}"#).is_none());
        assert!(parse_hello("not json").is_none());
        // A hello with an unusable id is still a hello — the label survives and
        // the row simply stays unidentified.
        let weak = parse_hello(r#"{"type":"hello","clientId":"x","label":"Phone"}"#).unwrap();
        assert_eq!(weak.id, None);
        assert_eq!(weak.label.as_deref(), Some("Phone"));
    }

    #[test]
    fn agents_are_summarised_as_a_hint() {
        assert_eq!(agent_hint(PHONE).as_deref(), Some("iPhone · Safari"));
        assert_eq!(
            agent_hint(
                "Mozilla/5.0 (Linux; Android 14) AppleWebKit/537.36 Chrome/120 Safari/537.36"
            )
            .as_deref(),
            Some("Android · Chrome")
        );
        assert_eq!(agent_hint("").is_none(), true);
        // Something we do not recognise is shown verbatim rather than guessed at.
        assert_eq!(agent_hint("websocat/1.0").as_deref(), Some("websocat/1.0"));
    }

    #[test]
    fn gone_rows_are_bounded() {
        let reg = registry();
        for n in 0..(RETAIN_DISCONNECTED + 5) {
            let handle = reg.connect(
                addr(&format!("192.168.0.{}:5000", n + 1)),
                None,
                ConnectingIdentity::Claimed(format!("device-{n:08}")),
            );
            drop(handle);
        }
        let view = reg.view();
        assert_eq!(view.connections, 0);
        assert_eq!(view.clients.len(), RETAIN_DISCONNECTED);
    }

    #[test]
    fn the_view_serialises_with_camel_case_and_a_tagged_browser_count() {
        let reg = registry();
        let _a = reg.connect(
            addr("192.168.0.31:5000"),
            Some(PHONE),
            ConnectingIdentity::Unidentified,
        );
        let v = serde_json::to_value(reg.view()).unwrap();
        assert_eq!(v["connections"], 1);
        assert_eq!(v["browsers"]["state"], "atLeast");
        assert_eq!(v["browsers"]["unidentified"], 1);
        assert_eq!(v["anyUnidentified"], true);
        assert!(v["clients"][0]["lastHeardMs"].is_number());
        assert!(v["clients"][0]["previousAddresses"].is_array());
        assert!(v["clients"][0]["id"].is_null());
    }
}
