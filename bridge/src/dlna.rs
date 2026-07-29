//! Browsing a DLNA media server from the phone, and playing from it.
//!
//! This is the transport and the state; the protocol lives in
//! [`crate::ssdp`] (finding servers), [`crate::upnp`] (describing and browsing
//! them) and [`crate::mediaproxy`] (moving the bytes).
//!
//! Three endpoints, all token-gated exactly like `/healthz` and `/library`:
//!
//! ```text
//! GET /dlna/index.json                       -> the servers, and why the list is what it is
//! GET /dlna/browse.json?server=&object=&…    -> one directory level, each item carrying the
//!                                               chosen <res>, why it was chosen, and whether
//!                                               it can be scrubbed
//! GET /dlna/media/<ref>                      -> the bytes, range-correct
//! ```
//!
//! The JSON shape follows `/library/index.json` deliberately — same
//! `configured` / `scannedAtMs` / `ageMs` / `generation` envelope — so the app
//! has one convention for "an index the bridge serves" rather than two.
//!
//! ## `<ref>` is opaque, and that is the security boundary
//!
//! The obvious design is `GET /dlna/media?url=http://…`, and it is wrong. It
//! turns the bridge into a general-purpose HTTP proxy for anything it can
//! reach: other machines on the LAN, services on localhost, the bridge's own
//! listener. The pairing token is not a sufficient answer, because that token
//! travels in a URL in cleartext until the TLS branch lands, and because the
//! phone's own page is the thing most likely to be tricked into asking.
//!
//! So a media URL is never accepted from a client. Browsing mints an opaque
//! reference for each `<res>` it saw, and `/dlna/media/<ref>` serves only
//! references this process minted. A URL the bridge has not itself received
//! from a media server, in a DIDL response, for an item under that server's
//! own ContentDirectory, is unreachable through this endpoint.
//!
//! ## What a malformed or hostile DIDL response can reach
//!
//! Answered concretely, because "we parse XML from the network" deserves a
//! concrete answer:
//!
//! - **A responder may only describe itself.** `LOCATION` is a URL supplied by
//!   anything on the network that can send a datagram, and an earlier version
//!   fetched it unchecked — a GET-forgery primitive against loopback and the
//!   LAN, fired every time a client opened the picker, with the outcome
//!   rendered back as `unreadable[].problem`. [`crate::ssdp`] now requires
//!   `LOCATION`'s host to be the datagram's source address, which is the one
//!   field a sender cannot choose.
//! - **A `<res>` URL on another host is refused at mint time.** It must match
//!   the host of the device description the server was discovered at. So a
//!   rogue media server — and anything on the network can answer an SSDP
//!   search — cannot make the bridge fetch `http://192.168.0.1/admin` or
//!   `http://127.0.0.1:8787/`, at either layer.
//! - **A `<res>` URL on another *port* of the same host is allowed.** Servers
//!   legitimately serve descriptions and media on different ports. The residual
//!   exposure is that a compromised media server can make the bridge fetch from
//!   other ports *on its own machine* — which is a machine the user chose to
//!   trust with their media, and is not reachable by anything else on the
//!   network through this path.
//! - **Only `http://` is fetchable at all.** [`crate::httpc::Url`] refuses
//!   every other scheme, and refuses a `user@host` authority.
//! - **Nothing unbounded is allocated.** XML is capped at
//!   [`crate::upnp::MAX_XML_BYTES`], items at a fixed count, and the reference
//!   table at [`MAX_REFS`]. Media is streamed, never buffered.
//! - **A malformed document loses items, not the process.** The DIDL parser is
//!   iterative, returns what parsed before the break, and has no `unwrap` on
//!   server-supplied data.
//!
//! The one thing a hostile server *can* still do is offer a file that is not
//! what it claims. It is a media server; that is inherent.
//!
//! ## Discovery is on demand, not on a timer
//!
//! `M-SEARCH` is multicast: every search costs every device on the network a
//! little work. A background poll would spray the LAN forever for a feature
//! nobody may be using. So discovery runs when asked, the result is cached for
//! [`DISCOVERY_TTL`], and `?refresh=1` forces a fresh one.

use std::collections::HashMap;
use std::time::Duration;

use serde::Serialize;
use tokio::sync::Mutex;

use crate::httpc::Url;
use crate::logging::now_ms;
use crate::ssdp::{self, Discovery};
use crate::upnp::{self, Device, Listing};
use crate::{log_debug, log_info, log_warn};

/// How long a discovery result is reused before another search goes out.
pub const DISCOVERY_TTL: Duration = Duration::from_secs(60);

/// Cap on the reference table. Each entry is a URL and an item title.
///
/// A user browsing a large library builds these up one page at a time; 20,000
/// is far past a session's worth and bounds the memory a client can make the
/// bridge hold by browsing in a loop.
const MAX_REFS: usize = 20_000;

/// One media reference: what `/dlna/media/<ref>` resolves to.
#[derive(Debug, Clone)]
struct MediaRef {
    upstream: Url,
    /// For the log line when it is played.
    title: String,
    /// The `<res>` MIME, already checked against a fixed list by
    /// [`crate::upnp::choose_res`]. Served downstream in place of whatever the
    /// upstream claims — see [`crate::mediaproxy`].
    mime: String,
    /// `size` from the `<res>` that produced this reference, when the server
    /// gave one.
    ///
    /// A second, independent statement of the file's length, which is what
    /// makes it useful: [`crate::mediaproxy`] will only synthesise a `416` when
    /// an origin's `Content-Length` agrees with it. See the comment there.
    advertised_size: Option<u64>,
    /// Insertion order, for eviction.
    seq: u64,
}

#[derive(Default)]
struct State {
    /// The last search, and when it finished.
    discovery: Option<Discovery>,
    discovered_at_ms: u64,
    /// Devices we successfully described, keyed by UDN.
    devices: HashMap<String, Device>,
    /// Description URLs supplied by hand rather than found by searching.
    ///
    /// Exists because discovery is the part most likely to fail for reasons
    /// nobody controls: Universal Media Server has an IP allowlist, multicast
    /// does not cross a VLAN or a mesh-router boundary, and a Windows box will
    /// happily send an `M-SEARCH` out of a WSL adapter. When the address is
    /// known, none of that has to work — and a bridge that can only be pointed
    /// at things it discovered is one that some networks simply cannot use.
    ///
    /// Re-described on every refresh, so a server that restarts on a new port
    /// still comes back, and so a manual entry that has gone away is reported
    /// as unreadable rather than silently kept.
    manual: Vec<Url>,
    /// Servers that answered the search but could not be described. Kept and
    /// reported rather than dropped: "found a media server, could not read its
    /// description" and "found nothing" are different problems and only one of
    /// them is about the network.
    undescribable: Vec<(String, String)>,
    refs: HashMap<String, MediaRef>,
    next_seq: u64,
    /// Bumped whenever the server list changes, matching `/library`'s use of
    /// the same field.
    generation: u64,
}

/// The DLNA surface's state. One per process.
pub struct Dlna {
    state: Mutex<State>,
}

impl Default for Dlna {
    fn default() -> Self {
        Self::new()
    }
}

impl Dlna {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State::default()),
        }
    }

    /// Discover servers, reusing a recent result unless `force`.
    ///
    /// Holds the lock across the search deliberately: two concurrent requests
    /// must not both multicast. The second waits and gets the first one's
    /// answer, which is the behaviour a phone with two tabs open needs.
    async fn ensure_discovered(&self, force: bool) {
        let mut state = self.state.lock().await;
        let age = now_ms().saturating_sub(state.discovered_at_ms);
        let fresh_enough =
            state.discovery.is_some() && age < DISCOVERY_TTL.as_millis() as u64 && !force;
        if fresh_enough {
            return;
        }

        // Manual entries are described whether or not a search finds anything,
        // and are described first so a discovered duplicate does not displace
        // the one the user asked for.
        let manual = state.manual.clone();
        let mut devices = HashMap::new();
        let mut undescribable = Vec::new();
        for location in &manual {
            match upnp::describe(location).await {
                Ok(device) => {
                    log_info!("[dlna] {} browsable at {} (configured by hand)", device.friendly_name, device.control_url);
                    devices.insert(device.udn.clone(), device);
                }
                Err(e) => {
                    log_warn!("[dlna] the configured server {location} could not be described: {e}");
                    undescribable.push((location.to_string(), e));
                }
            }
        }

        // A search still runs even with manual entries configured — a user with
        // one server pinned may have a second one they have not pinned.
        let discovery = ssdp::discover().await;
        for found in &discovery.servers {
            match upnp::describe(&found.location).await {
                Ok(device) => {
                    // **A second device claiming a UDN does not get it.**
                    //
                    // The UDN is the handle the client browses by, and this map
                    // is what resolves it. Anything on the network can answer
                    // an `M-SEARCH` and put any `<UDN>` in its own description,
                    // so overwriting on a repeat would let a rogue responder
                    // take over the identity of the real media server and
                    // receive every subsequent `Browse` — the pinned entries
                    // are described first precisely so they cannot be
                    // displaced, and that ordering only helps if a later
                    // insert loses.
                    //
                    // Wrong-scope rather than wrong-implementation: this loop
                    // was about *describing* devices, so nothing in it was
                    // about two of them claiming to be the same one.
                    if let Some(existing) = devices.get(&device.udn) {
                        if is_udn_collision(existing, &device) {
                            log_warn!(
                                "[dlna] {} claims UDN {}, which {} already holds; ignoring the                                  newcomer",
                                device.description_url,
                                device.udn,
                                existing.description_url
                            );
                            undescribable.push((
                                device.description_url.to_string(),
                                format!(
                                    "claims the same UDN ({}) as {}, which was found first;                                      ignored",
                                    device.udn, existing.description_url
                                ),
                            ));
                        }
                        continue;
                    }
                    log_info!(
                        "[dlna] {} ({}) browsable at {}",
                        device.friendly_name,
                        device.udn,
                        device.control_url
                    );
                    devices.insert(device.udn.clone(), device);
                }
                Err(e) => {
                    log_warn!("[dlna] {} answered the search but could not be described: {e}", found.location);
                    undescribable.push((found.location.to_string(), e));
                }
            }
        }

        let changed = devices.len() != state.devices.len()
            || devices.keys().any(|k| !state.devices.contains_key(k));
        if changed {
            state.generation += 1;
        }
        state.devices = devices;
        state.undescribable = undescribable;
        state.discovery = Some(discovery);
        state.discovered_at_ms = now_ms();
    }

    /// Register a media server by its description URL, bypassing discovery.
    ///
    /// Describes it immediately so a bad address is reported now rather than as
    /// an empty library later — which is the §0b failure this whole module is
    /// arranged around. Returns the device on success.
    ///
    /// **A failure still registers the address.** The first version returned
    /// early on a description failure, so a `--dlna-server` that could not be
    /// reached at startup was logged once and then vanished: it was absent from
    /// `/dlna/index.json`, absent from `unreadable`, and never retried. The
    /// user had configured a server and the API's answer was silence. On a
    /// headless service, where the startup log is the one thing nobody reads,
    /// that is the whole failure.
    ///
    /// Keeping it means it is reported every time the index is fetched, and
    /// retried on every refresh — which is also right for a server that is
    /// simply not switched on yet, the same reasoning [`State::manual`] gives
    /// for a network share that mounts after login.
    ///
    /// Found by exercising the failure path on purpose, per `FOLLOW-UPS.md`
    /// §0c: the success path had been run by every integration test and this
    /// one had only been reasoned about.
    pub async fn add_server(&self, location: Url) -> Result<Device, String> {
        let described = upnp::describe(&location).await;

        let mut state = self.state.lock().await;
        if !state.manual.contains(&location) {
            state.manual.push(location.clone());
        }

        let device = match described {
            Ok(device) => device,
            Err(problem) => {
                // Reported by `/dlna/index.json` until it starts working.
                if !state
                    .undescribable
                    .iter()
                    .any(|(loc, _)| loc == &location.to_string())
                {
                    state.undescribable.push((location.to_string(), problem.clone()));
                }
                // Force the next index fetch to re-describe rather than serving
                // a cached "no servers" for the next minute.
                state.discovered_at_ms = 0;
                return Err(problem);
            }
        };
        state.devices.insert(device.udn.clone(), device.clone());
        state.generation += 1;
        // A manual entry is itself a discovery result: without this, the first
        // `/dlna/index.json` would run a search purely because `discovery` was
        // still `None`.
        if state.discovery.is_none() {
            state.discovery = Some(Discovery::default());
        }
        state.discovered_at_ms = now_ms();
        Ok(device)
    }

    async fn device(&self, udn: &str) -> Option<Device> {
        self.state.lock().await.devices.get(udn).cloned()
    }

    /// Mint a reference for a `<res>` URL, refusing anything off the device's
    /// host. Returns `None` when refused.
    ///
    /// **Public deliberately, and it should stay the only door.** This is the
    /// one place a URL becomes reachable through `/dlna/media/…`, and the host
    /// check below is the boundary that stops a rogue SSDP responder pointing
    /// the bridge at `127.0.0.1:8787` or the router. Public and singular beats
    /// private with a second path added later: if you have found your way here
    /// because the check is in the way of something you want to make playable,
    /// the right move is to call this and satisfy the check, not to add a
    /// bypass beside it. A bypass would be invisible to every test that guards
    /// this one.
    ///
    /// Browsing is the only caller in the bridge; the throughput benchmark in
    /// `tests/dlna_media.rs` is the only other one, and it goes through the
    /// check like everything else.
    pub async fn mint_media_ref(
        &self,
        device: &Device,
        item_id: &str,
        title: &str,
        res_url: &str,
        advertised_size: Option<u64>,
        mime: &str,
    ) -> Option<String> {
        let upstream = Url::parse(res_url)?;
        if upstream.host != device.description_url.host {
            log_warn!(
                "[dlna] {} advertised media on another host ({}); refused",
                device.friendly_name,
                upstream.host
            );
            return None;
        }

        // Deterministic, so the same item keeps the same reference across
        // browses and a client can cache one. Not a security property — the
        // table lookup is what makes a reference valid, not its shape.
        let id = short_hash(&[device.udn.as_bytes(), item_id.as_bytes(), res_url.as_bytes()]);

        let mut state = self.state.lock().await;
        if let Some(existing) = state.refs.get(&id) {
            if existing.upstream == upstream {
                return Some(id);
            }
            // A 64-bit collision between two URLs on one server is not going to
            // happen, but serving the wrong file if it did is bad enough to be
            // worth one branch. Refusing rather than overwriting keeps whatever
            // is already playing playing.
            log_warn!("[dlna] reference collision on {id}; refusing the newcomer");
            return None;
        }

        if state.refs.len() >= MAX_REFS {
            // Evict the oldest. A reference that vanishes mid-playback produces
            // a 404 the client can recover from by re-browsing; an unbounded
            // map does not have a recovery.
            if let Some(oldest) = state
                .refs
                .iter()
                .min_by_key(|(_, r)| r.seq)
                .map(|(k, _)| k.clone())
            {
                state.refs.remove(&oldest);
            }
        }
        let seq = state.next_seq;
        state.next_seq += 1;
        state.refs.insert(
            id.clone(),
            MediaRef {
                upstream,
                // Capped. A title is server-supplied and this map holds up to
                // `MAX_REFS` of them; unbounded, a server could make the bridge
                // hold megabytes per entry. It is only used for a log line.
                title: title.chars().take(200).collect(),
                mime: mime.to_string(),
                advertised_size,
                seq,
            },
        );
        Some(id)
    }

    async fn resolve(&self, id: &str) -> Option<MediaRef> {
        self.state.lock().await.refs.get(id).cloned()
    }
}

/// Whether a newly described device is a *different* device claiming a UDN that
/// is already held.
///
/// Extracted so the decision is testable without two responders on a network:
/// the loop that calls it only runs after a real `M-SEARCH`, which is exactly
/// the sort of code that ends up asserted by nobody.
///
/// A repeat of the same device — one description URL answering twice, or a
/// pinned entry also being discovered — is not a collision and is silently
/// ignored. Two different URLs claiming one UDN is, and the first described
/// keeps it.
fn is_udn_collision(existing: &Device, candidate: &Device) -> bool {
    existing.description_url != candidate.description_url
}

/// FNV-1a over several byte strings, hex-rendered. Not a security primitive —
/// see [`Dlna::mint`].
fn short_hash(parts: &[&[u8]]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for (i, part) in parts.iter().enumerate() {
        // A separator, so ("ab","c") and ("a","bc") do not collide trivially.
        for b in part.iter().chain(std::iter::once(&(i as u8 | 0x80))) {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

// ---------------------------------------------------------------------------
// Responses
// ---------------------------------------------------------------------------

/// `GET /dlna/index.json`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct IndexResponse<'a> {
    servers: Vec<ServerSummary<'a>>,
    /// Always `true` — DLNA needs no configuration, unlike a library folder.
    /// Present so the two indexes have the same envelope.
    configured: bool,
    scanned_at_ms: u64,
    age_ms: u64,
    generation: u64,
    /// **Why the list is what it is.** `null` when servers were found.
    ///
    /// The whole point of this field: `FOLLOW-UPS.md` §0b is about a diagnostic
    /// that names the wrong subsystem, and an empty media-server list is a
    /// specimen — it reads as a statement about the network when the cause may
    /// be that the search never left the machine, or that Universal Media
    /// Server's IP allowlist is refusing this host. An empty list here always
    /// arrives with the evidence for which one it is.
    explanation: Option<String>,
    /// The raw evidence behind `explanation`.
    search: Option<&'a Discovery>,
    /// Servers that answered but could not be described.
    unreadable: Vec<UnreadableServer<'a>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerSummary<'a> {
    udn: &'a str,
    friendly_name: &'a str,
    model: Option<&'a str>,
    host: &'a str,
    /// `true` when this server was configured by address rather than found by
    /// searching. Worth showing: if the discovered list is empty and only the
    /// pinned server is present, discovery is not working and the user should
    /// know that before they add a second server and wonder why it is missing.
    configured_by_hand: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UnreadableServer<'a> {
    location: &'a str,
    problem: &'a str,
}

/// `GET /dlna/browse.json`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowseResponse {
    server: String,
    object_id: String,
    containers: Vec<upnp::Container>,
    items: Vec<BrowseItem>,
    start: u32,
    number_returned: u32,
    /// As the server reported it. Servers under-report; see [`upnp::Listing`].
    total_matches: u32,
    /// Whether another page is worth asking for. Derived from what actually
    /// arrived, not from `totalMatches`, because that field is unreliable and a
    /// client that trusts it stops early on a full library.
    more_available: bool,
    scanned_at_ms: u64,
    generation: u64,
}

/// An item as the client needs it: identity, a real title, and one URL to play.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowseItem {
    id: String,
    /// `dc:title`. A real title, not a filename with the spaces turned to
    /// dashes — which is the better input to *display*, and the reason the
    /// handoff document called this out.
    title: String,
    /// The last path segment of the chosen `<res>` URL, percent-decoded.
    ///
    /// Carried because `mediaUrl` is deliberately opaque, and the client's
    /// script matching compares a funscript's stem against a media *basename*.
    /// Without this the reference hash would be the only name the client had,
    /// and every DLNA item would silently match nothing.
    ///
    /// So the handoff document's suggestion that `dc:title` might replace the
    /// naming work is half right: the title is better to show and worse to
    /// match on, at least until stem comparison normalises spaces and dashes.
    /// Both travel, so the client can move when it is ready.
    file_name: Option<String>,
    class: String,
    /// `/dlna/media/<ref>` — relative, so it inherits the page's origin and
    /// therefore its scheme. `null` when nothing playable was on offer.
    media_url: Option<String>,
    /// Why that resource, in one line.
    chosen: Option<String>,
    /// `false` when the server said it does not honour byte ranges. The client
    /// should say scrubbing will not work **and say the media server is why**,
    /// rather than letting it look like the bridge dropping seeks.
    seekable: bool,
    /// Present only when nothing playable was found: what was on offer, so the
    /// answer to "why is this file missing" is in the listing.
    unplayable: Option<String>,
    duration: Option<String>,
    resolution: Option<String>,
    size: Option<u64>,
}

// ---------------------------------------------------------------------------
// Routing
// ---------------------------------------------------------------------------

/// What the HTTP layer should do with a `/dlna/…` request.
///
/// Returned rather than written, so [`crate::http`] keeps ownership of how a
/// buffered response is framed and this module does not need a second copy of
/// [`crate::http::respond`]. The media arm is the exception, because streaming
/// is the whole point of it.
pub enum Action {
    /// A complete JSON body.
    Json(Vec<u8>),
    /// A status and a plain-text message.
    Error(u16, String),
    /// Stream this upstream URL, honouring the client's range headers.
    Stream {
        upstream: Url,
        title: String,
        /// What the `<res>` said the file is, if anything. Corroboration for
        /// the origin's own `Content-Length`.
        advertised_size: Option<u64>,
        /// The validated `<res>` MIME, served downstream in place of the
        /// upstream's own claim.
        mime: String,
    },
}

/// Route one `/dlna/…` request. `rest` is the path after `/dlna/`.
pub async fn handle(dlna: &Dlna, rest: &str, query: &str) -> Action {
    let params = parse_query(query);
    match rest {
        "index.json" => {
            let force = params.get("refresh").is_some_and(|v| v == "1");
            index(dlna, force).await
        }
        "browse.json" => browse(dlna, &params).await,
        _ => match rest.strip_prefix("media/") {
            Some(reference) => match dlna.resolve(reference).await {
                Some(m) => Action::Stream {
                    upstream: m.upstream,
                    title: m.title,
                    advertised_size: m.advertised_size,
                    mime: m.mime,
                },
                // §0b: say what is actually missing. A stale reference after a
                // restart is the common case and "not found" alone sends people
                // looking at the media server.
                None => Action::Error(
                    404,
                    "no such media reference. References are minted by browsing and do not \
                     survive a bridge restart — re-open the library and pick the item again."
                        .into(),
                ),
            },
            None => Action::Error(404, "not found".into()),
        },
    }
}

async fn index(dlna: &Dlna, force: bool) -> Action {
    dlna.ensure_discovered(force).await;
    let state = dlna.state.lock().await;

    let mut servers: Vec<ServerSummary> = state
        .devices
        .values()
        .map(|d| ServerSummary {
            udn: &d.udn,
            friendly_name: &d.friendly_name,
            model: d.model.as_deref(),
            host: &d.description_url.host,
            configured_by_hand: state.manual.contains(&d.description_url),
        })
        .collect();
    servers.sort_by(|a, b| a.friendly_name.cmp(b.friendly_name));

    // The explanation covers "no servers" and also "servers answered but none
    // could be described", which the SSDP layer cannot know about.
    let explanation = if !servers.is_empty() {
        None
    } else if !state.undescribable.is_empty() {
        Some(format!(
            "{} device(s) answered the media-server search but their descriptions could not be \
             read, so there is nothing to browse. This is the media server refusing or \
             misanswering, not an empty network: {}",
            state.undescribable.len(),
            state
                .undescribable
                .iter()
                .map(|(loc, why)| format!("{loc}: {why}"))
                .collect::<Vec<_>>()
                .join("; ")
        ))
    } else {
        state.discovery.as_ref().and_then(|d| d.empty_explanation())
    };

    let body = IndexResponse {
        servers,
        configured: true,
        scanned_at_ms: state.discovered_at_ms,
        age_ms: now_ms().saturating_sub(state.discovered_at_ms),
        generation: state.generation,
        explanation,
        search: state.discovery.as_ref(),
        unreadable: state
            .undescribable
            .iter()
            .map(|(loc, why)| UnreadableServer {
                location: loc,
                problem: why,
            })
            .collect(),
    };
    Action::Json(serde_json::to_vec(&body).unwrap_or_else(|_| b"{\"servers\":[]}".to_vec()))
}

async fn browse(dlna: &Dlna, params: &HashMap<String, String>) -> Action {
    dlna.ensure_discovered(false).await;

    let Some(udn) = params.get("server") else {
        return Action::Error(400, "browse.json needs ?server=<udn>".into());
    };
    let Some(device) = dlna.device(udn).await else {
        return Action::Error(
            404,
            format!("no media server with UDN {udn:?} is currently known. Fetch \
                     /dlna/index.json?refresh=1 — the server may have gone away or the bridge may \
                     have restarted."),
        );
    };

    let object_id = params
        .get("object")
        .cloned()
        .unwrap_or_else(|| upnp::ROOT_OBJECT.to_string());
    let start: u32 = params.get("start").and_then(|v| v.parse().ok()).unwrap_or(0);
    let count: u32 = params
        .get("count")
        .and_then(|v| v.parse().ok())
        .unwrap_or(upnp::PAGE_SIZE)
        .clamp(1, upnp::PAGE_SIZE);

    let listing: Listing = match upnp::browse(&device.control_url, &object_id, start, count).await {
        Ok(l) => l,
        Err(e) => {
            // The message already names the media server and the object; it is
            // relayed rather than replaced, because a generic "browse failed"
            // here is exactly the misattribution §0b is about.
            log_warn!("[dlna] {e}");
            return Action::Error(502, e);
        }
    };

    let mut items = Vec::with_capacity(listing.items.len());
    for item in &listing.items {
        let chosen = upnp::choose_res(item);
        let file_name = chosen.as_ref().ok().and_then(|c| file_name_of(&c.res.url));
        let (media_url, reason, seekable, unplayable, duration, resolution, size) = match chosen {
            Ok(c) => {
                let minted = dlna
                    .mint_media_ref(
                        &device,
                        &item.id,
                        &item.title,
                        &c.res.url,
                        c.res.size,
                        c.res.mime(),
                    )
.await;
                let unplayable = if minted.is_none() {
                    Some(format!(
                        "the chosen resource is on {}, which is not the host this server was \
                         discovered at ({}); refused",
                        Url::parse(&c.res.url)
                            .map(|u| u.host)
                            .unwrap_or_else(|| "an unparseable URL".into()),
                        device.description_url.host
                    ))
                } else {
                    None
                };
                (
                    minted.map(|id| format!("/dlna/media/{id}")),
                    Some(c.reason),
                    c.seekable,
                    unplayable,
                    c.res.duration.clone(),
                    c.res.resolution.clone(),
                    c.res.size,
                )
            }
            Err(why) => (None, None, false, Some(why), None, None, None),
        };
        items.push(BrowseItem {
            id: item.id.clone(),
            title: item.title.clone(),
            file_name,
            class: item.class.clone(),
            media_url,
            chosen: reason,
            seekable,
            unplayable,
            duration,
            resolution,
            size,
        });
    }

    let returned = (listing.containers.len() + listing.items.len()) as u32;
    // Derived from what arrived. `totalMatches` is unreliable — UMS reports 0
    // for some containers — and a client that loops on it stops after one page
    // on a full library.
    let more_available = returned >= count;

    let body = BrowseResponse {
        server: device.udn.clone(),
        object_id,
        containers: listing.containers,
        items,
        start,
        number_returned: listing.number_returned,
        total_matches: listing.total_matches,
        more_available,
        scanned_at_ms: now_ms(),
        generation: dlna.state.lock().await.generation,
    };
    log_debug!(
        "[dlna] {} browse -> {} entries",
        device.friendly_name,
        returned
    );
    Action::Json(serde_json::to_vec(&body).unwrap_or_else(|_| b"{\"items\":[]}".to_vec()))
}

/// The last path segment of a media URL, percent-decoded.
///
/// What the client matches funscripts against — see [`BrowseItem::file_name`].
/// A query string is stripped first: UMS puts none there, but a server that
/// does would otherwise hand back `x.mp4?profile=2` as the name.
fn file_name_of(res_url: &str) -> Option<String> {
    let path = Url::parse(res_url)?.path_and_query;
    let path = path.split(['?', '#']).next().unwrap_or("");
    let last = path.rsplit('/').find(|s| !s.is_empty())?;
    let decoded = percent_decode(last);
    (!decoded.is_empty()).then_some(decoded)
}

/// Parse a query string into a map, percent-decoding both halves.
fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter(|s| !s.is_empty())
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            Some((percent_decode(k), percent_decode(v)))
        })
        .collect()
}

/// Percent-decode, with `+` as a space.
///
/// `ObjectID`s from UMS contain `$` and can contain spaces and slashes, so they
/// arrive encoded and must be decoded before they go back into a SOAP body.
/// Invalid escapes are left as written rather than dropped — losing a character
/// silently would produce an ObjectID that browses the wrong folder.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    //! Nothing here opens a socket. [`Dlna::ensure_discovered`] is the only
    //! function in this module that reaches the network and it is not called
    //! by any test — see [`crate::ssdp`]'s test module for why this crate
    //! writes that down.

    use super::*;
    use crate::upnp::{Item, Res};

    fn device() -> Device {
        Device {
            udn: "uuid:06b1f0ee".into(),
            friendly_name: "Universal Media Server".into(),
            model: Some("UMS".into()),
            control_url: Url::parse("http://192.168.0.4:5001/upnp/control/content_directory")
                .unwrap(),
            description_url: Url::parse("http://192.168.0.4:5001/description/fetch").unwrap(),
        }
    }

    /// The security boundary, stated as a test. A media URL that a client
    /// invents cannot be played, because only references the bridge minted
    /// resolve.
    #[tokio::test]
    async fn only_minted_references_resolve() {
        let dlna = Dlna::new();
        let d = device();
        let id = dlna
            .mint_media_ref(&d, "1$7$253", "Ep I", "http://192.168.0.4:5001/ums/media/x.mp4", None, "video/mp4")
.await
            .unwrap();

        assert!(dlna.resolve(&id).await.is_some());
        assert!(dlna.resolve("0000000000000000").await.is_none());
        assert!(dlna.resolve("http://192.168.0.1/admin").await.is_none());
    }

    /// **The reach question, answered as a test.** A media server can put
    /// anything in a `<res>`; it cannot put another host in one.
    #[tokio::test]
    async fn a_res_url_on_another_host_is_refused() {
        let dlna = Dlna::new();
        let d = device();
        for hostile in [
            "http://127.0.0.1:8787/pair",
            "http://192.168.0.1/admin",
            "http://169.254.169.254/latest/meta-data/",
            "https://example.com/x.mp4",
            "file:///C:/Windows/win.ini",
            "http://trusted@192.168.0.99/x.mp4",
        ] {
            assert!(
                dlna.mint_media_ref(&d, "1", "t", hostile, None, "video/mp4")
.await.is_none(),
                "should refuse {hostile}"
            );
        }
        // The same host on another port is allowed: servers really do split
        // description and media across ports.
        assert!(dlna
            .mint_media_ref(&d, "1", "t", "http://192.168.0.4:9001/media/x.mp4", None, "video/mp4")
.await
            .is_some());
    }

    /// Minting is deterministic, so a client can hold a reference across a
    /// re-browse without the URL changing under it.
    #[tokio::test]
    async fn the_same_item_mints_the_same_reference() {
        let dlna = Dlna::new();
        let d = device();
        let a = dlna.mint_media_ref(&d, "1$7$253", "Ep I", "http://192.168.0.4:5001/a.mp4", None, "video/mp4")
.await;
        let b = dlna.mint_media_ref(&d, "1$7$253", "Ep I", "http://192.168.0.4:5001/a.mp4", None, "video/mp4")
.await;
        assert_eq!(a, b);

        let other = dlna.mint_media_ref(&d, "1$7$254", "Ep II", "http://192.168.0.4:5001/b.mp4", None, "video/mp4")
.await;
        assert_ne!(a, other);
    }

    /// A title is server-supplied and the reference table holds up to
    /// `MAX_REFS` of them. It is only ever used for a log line, so it is capped
    /// rather than trusted.
    #[tokio::test]
    async fn a_stored_title_is_capped() {
        let dlna = Dlna::new();
        let d = device();
        let huge = "A".repeat(100_000);
        let id = dlna
            .mint_media_ref(&d, "1", &huge, "http://192.168.0.4:5001/x.mp4", None, "video/mp4")
            .await
            .unwrap();
        assert!(dlna.resolve(&id).await.unwrap().title.chars().count() <= 200);
    }

    #[tokio::test]
    async fn the_reference_table_is_bounded() {
        let dlna = Dlna::new();
        let d = device();
        for i in 0..(MAX_REFS + 50) {
            dlna.mint_media_ref(&d, &format!("id{i}"), "t", &format!("http://192.168.0.4:5001/{i}.mp4"), None, "video/mp4")
                .await;
        }
        assert!(dlna.state.lock().await.refs.len() <= MAX_REFS);
    }

    /// A pinned server that cannot be described must not vanish.
    ///
    /// Regression: `add_server` used to return early on a description failure,
    /// so a `--dlna-server` that was unreachable at startup was logged once and
    /// then absent from `/dlna/index.json` entirely — not in `servers`, not in
    /// `unreadable`, and never retried. The user had configured a server and
    /// the API said nothing at all about it.
    ///
    /// This test dials port 1 on loopback, which nothing listens on, so it
    /// reaches no network. See this module's test-module note.
    #[tokio::test]
    async fn a_pinned_server_that_fails_is_still_reported() {
        let dlna = Dlna::new();
        let bad = Url::parse("http://127.0.0.1:1/nothing").unwrap();
        assert!(dlna.add_server(bad.clone()).await.is_err());

        let state = dlna.state.lock().await;
        assert!(
            state.manual.contains(&bad),
            "it must be retried, not forgotten"
        );
        assert!(
            state.undescribable.iter().any(|(loc, _)| loc == &bad.to_string()),
            "it must be reported: {:?}",
            state.undescribable
        );
        assert_eq!(
            state.discovered_at_ms, 0,
            "the next index fetch must re-describe rather than serve a cached empty answer"
        );
    }

    /// A reference that has been evicted, or that predates a restart, gets a
    /// message naming the actual cause rather than a bare 404 that sends
    /// someone to look at the media server.
    #[tokio::test]
    async fn an_unknown_reference_explains_itself() {
        let dlna = Dlna::new();
        let Action::Error(status, msg) = handle(&dlna, "media/deadbeef", "").await else {
            panic!("expected an error");
        };
        assert_eq!(status, 404);
        assert!(msg.contains("bridge restart") || msg.contains("restart"), "{msg}");
    }

    /// **A second device does not get to take the first one's UDN.**
    ///
    /// The UDN is the handle a client browses by, and the device map resolves
    /// it. Anything on the network can answer an `M-SEARCH` and put any `<UDN>`
    /// in its own description, so overwriting on a repeat would let a rogue
    /// responder inherit the real media server's identity and receive every
    /// subsequent `Browse`.
    ///
    /// Wrong-scope rather than wrong-implementation: the loop was about
    /// *describing* devices, so nothing in it was about two of them claiming to
    /// be the same one.
    #[test]
    fn a_second_device_claiming_a_held_udn_is_a_collision() {
        let real = device();
        let mut impostor = device();
        impostor.description_url =
            Url::parse("http://192.168.0.99:5001/description/fetch").unwrap();
        impostor.control_url = Url::parse("http://192.168.0.99:5001/ctrl").unwrap();
        impostor.friendly_name = "Also Universal Media Server".into();

        assert!(is_udn_collision(&real, &impostor));
        assert!(is_udn_collision(&impostor, &real));

        // The same device answering twice is not a collision — a pinned entry
        // is also discovered, and that must stay silent.
        assert!(!is_udn_collision(&real, &device()));
    }

    #[test]
    fn separates_hash_inputs() {
        // Without a separator these two would hash identically, and two
        // different items would share a reference.
        assert_ne!(
            short_hash(&[b"ab", b"c"]),
            short_hash(&[b"a", b"bc"])
        );
    }

    /// The name script matching needs, which the opaque media reference hides.
    ///
    /// Without this the client's only name for a DLNA item would be a hash, and
    /// every item would silently match no funscript at all.
    #[test]
    fn recovers_the_filename_from_the_res_url() {
        assert_eq!(
            file_name_of("http://192.168.0.4:5001/ums/media/06b1f0ee/253/Cock-Hero-Island-5-Episode-I.mp4"),
            Some("Cock-Hero-Island-5-Episode-I.mp4".into())
        );
        // Percent-encoded spaces come back as spaces, because that is what the
        // funscript beside it is named.
        assert_eq!(
            file_name_of("http://h/a/My%20Movie.mp4"),
            Some("My Movie.mp4".into())
        );
        // A query string is not part of the name.
        assert_eq!(file_name_of("http://h/a/x.mp4?profile=2"), Some("x.mp4".into()));
        assert_eq!(file_name_of("http://h/"), None);
        assert_eq!(file_name_of("not a url"), None);
    }

    #[test]
    fn decodes_object_ids_that_contain_dollars_and_spaces() {
        let q = parse_query("server=uuid%3Ax&object=1%247%24253&start=0");
        assert_eq!(q.get("server").unwrap(), "uuid:x");
        assert_eq!(q.get("object").unwrap(), "1$7$253");
        assert_eq!(q.get("start").unwrap(), "0");

        assert_eq!(percent_decode("a+b%20c"), "a b c");
        // A malformed escape is kept, not dropped — a lost character would
        // silently browse a different folder.
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }

    #[tokio::test]
    async fn an_index_with_no_servers_explains_why_not() {
        let dlna = Dlna::new();
        // Pre-load a discovery result rather than running one, so this test
        // touches no network.
        {
            let mut state = dlna.state.lock().await;
            state.discovery = Some(Discovery {
                bound_to: Some("192.168.0.9:5000".into()),
                bound_to_specific_interface: true,
                searches_sent: 3,
                replies_seen: 2,
                other_service_types: vec!["upnp:rootdevice".into()],
                ..Default::default()
            });
            state.discovered_at_ms = now_ms();
        }
        let Action::Json(body) = index(&dlna, false).await else {
            panic!("expected JSON");
        };
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["servers"].as_array().unwrap().len(), 0);
        let explanation = v["explanation"].as_str().unwrap();
        assert!(explanation.contains("allowlist"), "{explanation}");
        // The evidence travels with the claim.
        assert_eq!(v["search"]["searchesSent"], 3);
        assert_eq!(v["search"]["repliesSeen"], 2);
    }

    /// A server that was found but could not be described must not read as
    /// "nothing on the network".
    #[tokio::test]
    async fn an_undescribable_server_is_not_reported_as_an_empty_network() {
        let dlna = Dlna::new();
        {
            let mut state = dlna.state.lock().await;
            state.discovery = Some(Discovery {
                searches_sent: 3,
                replies_seen: 1,
                ..Default::default()
            });
            state.undescribable = vec![(
                "http://192.168.0.4:5001/description/fetch".into(),
                "answered 403 to a description fetch".into(),
            )];
            state.discovered_at_ms = now_ms();
        }
        let Action::Json(body) = index(&dlna, false).await else {
            panic!("expected JSON");
        };
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let explanation = v["explanation"].as_str().unwrap();
        assert!(explanation.contains("not an empty network"), "{explanation}");
        assert_eq!(v["unreadable"][0]["problem"], "answered 403 to a description fetch");
    }

    /// The listing envelope matches `/library/index.json`'s, so the app has one
    /// convention for a bridge-served index rather than two.
    #[tokio::test]
    async fn the_index_envelope_matches_the_library_convention() {
        let dlna = Dlna::new();
        {
            let mut state = dlna.state.lock().await;
            state.discovery = Some(Discovery::default());
            state.discovered_at_ms = now_ms();
        }
        let Action::Json(body) = index(&dlna, false).await else {
            panic!("expected JSON");
        };
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        for field in ["configured", "scannedAtMs", "ageMs", "generation"] {
            assert!(!v[field].is_null(), "missing {field}");
        }
    }

    /// An item whose only offer is unplayable still appears in the listing,
    /// carrying the reason. Dropping it would produce a library with holes in
    /// it and no way to find out why.
    #[test]
    fn an_unplayable_item_keeps_its_reason() {
        let item = Item {
            id: "1".into(),
            title: "Old AVI".into(),
            class: "object.item.videoItem".into(),
            resources: vec![Res {
                url: "http://192.168.0.4:5001/x.avi".into(),
                protocol_info: "http-get:*:video/x-msvideo:".into(),
                size: None,
                duration: None,
                resolution: None,
            }],
        };
        let err = upnp::choose_res(&item).unwrap_err();
        assert!(err.contains("video/x-msvideo"), "{err}");
    }
}
