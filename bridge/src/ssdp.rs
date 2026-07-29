//! SSDP discovery — the one thing a browser can never do.
//!
//! Finding a media server means sending `M-SEARCH` to the multicast address
//! `239.255.255.250:1900` and collecting the **unicast** replies. JavaScript has
//! no UDP primitive and will not get one; WebRTC's UDP is confined to ICE and
//! data channels. That is why this lives in the bridge.
//!
//! ## Why there is no new dependency here
//!
//! Replies to `M-SEARCH` come back unicast, to the source address of the
//! search. So this socket only ever *sends* to a multicast group and *receives*
//! ordinary datagrams — it never joins the group, which is the operation that
//! would need `socket2`. `tokio::net::UdpSocket` is enough.
//!
//! ## The interface problem, which is real on this machine
//!
//! A multicast send from a socket bound to `0.0.0.0` goes out whichever
//! interface the routing table prefers. On a development Windows box that is
//! frequently a WSL or Hyper-V virtual adapter, and the search then reaches
//! nothing. `FOLLOW-UPS.md` §0b records exactly this failure once already, for
//! mDNS: `coyote.local` resolved to the WSL adapter and the certificate got the
//! blame.
//!
//! So the socket is bound to the address [`crate::http::local_ip`] picks — the
//! interface the OS would use to reach the network — which makes the multicast
//! egress deterministic on the same basis the pairing QR already relies on. If
//! that returns nothing, `0.0.0.0` is the fallback and [`Discovery::bound_to`]
//! records which happened, because "we searched the wrong network" and "there
//! is nothing on this network" produce identical empty lists otherwise.
//!
//! ## Empty is not a result, it is three results
//!
//! This module refuses to hand back a bare empty list. `FOLLOW-UPS.md` §0b is
//! about a diagnostic that names the wrong subsystem, and "no media servers
//! found" is a perfect specimen: it reads as a statement about the network when
//! the cause is just as likely to be that our datagrams never left the machine.
//! [`Discovery`] therefore carries what was *done* — where it sent from, how
//! many searches went out, how long it listened, and how many SSDP replies of
//! **any** kind came back. A reply from some unrelated device proves multicast
//! works and the absence is genuine; zero replies of any kind proves nothing
//! about media servers at all.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use serde::Serialize;
use tokio::net::UdpSocket;

use crate::httpc::Url;
use crate::{log_debug, log_info, log_warn};

/// The SSDP multicast group and port. Fixed by the protocol.
const SSDP_ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(239, 255, 255, 250)), 1900);

/// The device type we are looking for.
pub const MEDIA_SERVER: &str = "urn:schemas-upnp-org:device:MediaServer:1";

/// How many `M-SEARCH` datagrams to send.
///
/// UDP is lossy and SSDP has no retransmission of its own; the protocol expects
/// searchers to repeat. Three is the conventional count.
const SEARCHES: usize = 3;

/// Gap between the repeats.
const SEARCH_GAP: Duration = Duration::from_millis(300);

/// `MX` — the maximum number of seconds a responder may wait before replying.
/// Responders randomise within it to avoid a stampede, so the listen window
/// must exceed it.
const MX_SECONDS: u32 = 2;

/// How long to listen after the last search.
const LISTEN: Duration = Duration::from_millis(2500);

/// Refuse a datagram larger than this. SSDP replies are a few hundred bytes.
const MAX_DATAGRAM: usize = 4096;

/// Cap on how many distinct responders are tracked in one search, so a host
/// spraying forged replies cannot make this allocate without limit.
const MAX_RESPONDERS: usize = 64;

/// One responder that claimed to be a media server.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Found {
    /// `USN`, the unique service name. Stable across restarts for a given
    /// server, which is what makes it usable as a handle the client can hold.
    pub usn: String,
    /// The `LOCATION` header: where the device description lives.
    #[serde(serialize_with = "url_as_string")]
    pub location: Url,
    /// `SERVER`, if given. Free text — "UMS/14.10.0 UPnP/1.0" and similar.
    pub server: Option<String>,
    /// Where the datagram actually came from.
    ///
    /// `location`'s host is now required to be this address — see
    /// [`location_is_the_sender`] — so the two always agree. Kept because it is
    /// the port and the evidence: it is the field that makes the check
    /// checkable.
    pub from: String,
}

fn url_as_string<S: serde::Serializer>(u: &Url, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&u.to_string())
}

/// The outcome of one search, including the evidence that it happened.
///
/// Every field except `servers` exists to stop an empty `servers` from being
/// reported as a fact about the network. See the module docs.
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Discovery {
    pub servers: Vec<Found>,
    /// The local address the search went out from, or `null` if the socket
    /// could not be bound at all.
    pub bound_to: Option<String>,
    /// Whether `bound_to` is a specific interface or the `0.0.0.0` fallback.
    /// The fallback is the case that silently searches a virtual adapter.
    pub bound_to_specific_interface: bool,
    /// How many `M-SEARCH` datagrams were actually written to the socket.
    pub searches_sent: usize,
    /// How long we listened, in milliseconds.
    pub listened_ms: u64,
    /// Every SSDP reply received, including ones that were not media servers.
    ///
    /// **This is the field that distinguishes the two empty cases.** Non-zero
    /// with `servers` empty means multicast works and there genuinely is no
    /// media server. Zero means we learned nothing — quite possibly the
    /// datagrams never left this machine.
    pub replies_seen: usize,
    /// Replies that were SSDP but advertised something else. Their `ST` values,
    /// deduplicated, capped — enough to show the search was heard.
    pub other_service_types: Vec<String>,
    /// Anything that went wrong at the socket layer, in the order it happened.
    pub problems: Vec<String>,
}

impl Discovery {
    /// A one-line, human-facing account of *why* the list is empty.
    ///
    /// Returns `None` when servers were found, because then there is nothing to
    /// explain. The strings deliberately name the component that could actually
    /// be at fault, per `FOLLOW-UPS.md` §0b — including "not us, and not the
    /// network either".
    pub fn empty_explanation(&self) -> Option<String> {
        if !self.servers.is_empty() {
            return None;
        }
        if self.bound_to.is_none() {
            return Some(
                "No search was sent: the bridge could not open a UDP socket. This is a local \
                 problem, not a network one."
                    .into(),
            );
        }
        if self.searches_sent == 0 {
            return Some(format!(
                "No search left the machine — every send failed from {}. {}",
                self.bound_to.as_deref().unwrap_or("?"),
                self.problems.join("; ")
            ));
        }
        if self.replies_seen == 0 {
            let iface = if self.bound_to_specific_interface {
                format!(
                    "sent from {}",
                    self.bound_to.as_deref().unwrap_or("?")
                )
            } else {
                "sent from the default route (0.0.0.0), which on a machine with virtual adapters \
                 may not be the network your media server is on"
                    .to_string()
            };
            return Some(format!(
                "{} searches went out ({}), and nothing on the network answered at all — not even \
                 devices that are not media servers. That points at the search not reaching the \
                 network rather than at an absence of media servers.",
                self.searches_sent, iface
            ));
        }
        Some(format!(
            "{} devices answered the search, but none of them is a UPnP MediaServer (saw: {}). \
             The search is reaching the network, so this is an absence of media servers — or a \
             media server that is refusing this host. Universal Media Server has an IP allowlist; \
             if it is running, check that {} is on it.",
            self.replies_seen,
            if self.other_service_types.is_empty() {
                "no service types".to_string()
            } else {
                self.other_service_types.join(", ")
            },
            self.bound_to.as_deref().unwrap_or("this machine"),
        ))
    }
}

/// Send `M-SEARCH` and collect media-server replies.
///
/// Never returns an error: a search that fails is a [`Discovery`] that says so,
/// because the caller has to render *something* and "the search failed" is a
/// more useful thing to render than a generic error.
pub async fn discover() -> Discovery {
    let mut out = Discovery::default();

    // Bind to the interface the OS would use to reach the network. See the
    // module docs for why `0.0.0.0` is the fallback and not the default.
    let (socket, specific) = match crate::http::local_ip() {
        Some(ip) => match UdpSocket::bind(SocketAddr::new(ip, 0)).await {
            Ok(s) => (Some(s), true),
            Err(e) => {
                out.problems
                    .push(format!("could not bind to {ip}: {e}; falling back to 0.0.0.0"));
                (UdpSocket::bind("0.0.0.0:0").await.ok(), false)
            }
        },
        None => {
            out.problems
                .push("no routable local address; falling back to 0.0.0.0".into());
            (UdpSocket::bind("0.0.0.0:0").await.ok(), false)
        }
    };

    let Some(socket) = socket else {
        out.problems.push("could not open a UDP socket at all".into());
        log_warn!("[ssdp] no UDP socket; discovery cannot run");
        return out;
    };
    out.bound_to = socket.local_addr().ok().map(|a| a.to_string());
    out.bound_to_specific_interface = specific;

    // `HOST` must be the multicast address literally, not our own. `MAN` is
    // quoted, which is not optional — some responders reject an unquoted one.
    let probe = format!(
        "M-SEARCH * HTTP/1.1\r\n\
         HOST: 239.255.255.250:1900\r\n\
         MAN: \"ssdp:discover\"\r\n\
         MX: {MX_SECONDS}\r\n\
         ST: {MEDIA_SERVER}\r\n\
         USER-AGENT: coyote-bridge/{}\r\n\r\n",
        env!("CARGO_PKG_VERSION"),
    );

    for i in 0..SEARCHES {
        match socket.send_to(probe.as_bytes(), SSDP_ADDR).await {
            Ok(_) => out.searches_sent += 1,
            Err(e) => out.problems.push(format!("send {} failed: {e}", i + 1)),
        }
        if i + 1 < SEARCHES {
            tokio::time::sleep(SEARCH_GAP).await;
        }
    }
    if out.searches_sent == 0 {
        log_warn!("[ssdp] every M-SEARCH send failed: {}", out.problems.join("; "));
        return out;
    }

    let started = tokio::time::Instant::now();
    let deadline = started + LISTEN;
    // Keyed by USN so a server answering all three searches counts once.
    let mut by_usn: HashMap<String, Found> = HashMap::new();
    let mut other_types: Vec<String> = Vec::new();
    let mut buf = vec![0u8; MAX_DATAGRAM];

    loop {
        let recv = tokio::time::timeout_at(deadline, socket.recv_from(&mut buf)).await;
        let (n, from) = match recv {
            Ok(Ok(v)) => v,
            // A single ICMP-port-unreachable can surface here on Windows;
            // it is not a reason to stop listening to everyone else.
            Ok(Err(e)) => {
                out.problems.push(format!("recv failed: {e}"));
                continue;
            }
            Err(_) => break,
        };
        out.replies_seen += 1;
        let text = String::from_utf8_lossy(&buf[..n]);

        let st = header(&text, "st").unwrap_or_default();
        if st != MEDIA_SERVER {
            // Kept as proof the search was heard, not because we want them.
            if !st.is_empty() && !other_types.iter().any(|s| s == st) && other_types.len() < 8 {
                other_types.push(st.to_string());
            }
            log_debug!("[ssdp] {from} answered with ST {st:?}, not a media server");
            continue;
        }

        let Some(location_raw) = header(&text, "location") else {
            out.problems
                .push(format!("{from} answered as a MediaServer with no LOCATION"));
            continue;
        };
        let Some(location) = Url::parse(location_raw) else {
            // Refused rather than coerced. See `Url::parse`.
            out.problems.push(format!(
                "{from} gave a LOCATION this bridge will not follow: {location_raw:?}"
            ));
            continue;
        };
        // **A responder may only describe itself.**
        //
        // Without this, `LOCATION` is a URL supplied by anything on the network
        // that can send a UDP datagram, and the bridge fetches it on every
        // discovery — which is every time a client opens the picker. That is a
        // GET-forgery primitive against loopback and the LAN, and it comes with
        // an oracle attached: the outcome lands in `undescribable` and is
        // rendered verbatim as `unreadable[].problem` in `/dlna/index.json`, so
        // the sender can distinguish a 403 from a refused connection.
        //
        // The `<res>` gate one layer down was already fail-closed, so this was
        // never a way to make the bridge *proxy* those bytes. It was still a
        // way to make it fetch them, which is exactly what the module docs
        // claimed it could not do.
        //
        // The datagram's source address is the one thing here a sender cannot
        // choose: replies to `M-SEARCH` are unicast to an ephemeral port, so a
        // forged source would not have reached this socket. Comparing against
        // it is therefore the whole check.
        if !location_is_the_sender(&location, &from) {
            out.problems.push(format!(
                "{from} advertised a LOCATION on another host ({}); refused, because a \
                 responder may only describe itself",
                location.host
            ));
            log_warn!(
                "[ssdp] {from} advertised {location_raw:?}, which is not its own address; refused"
            );
            continue;
        }

        // USN is the identity. Without one there is nothing stable to key on,
        // so fall back to the location — which at least deduplicates.
        let usn = header(&text, "usn")
            .filter(|s| !s.is_empty())
            .unwrap_or(location_raw)
            .to_string();

        if by_usn.len() >= MAX_RESPONDERS && !by_usn.contains_key(&usn) {
            out.problems
                .push(format!("more than {MAX_RESPONDERS} responders; ignoring the rest"));
            break;
        }

        by_usn.entry(usn.clone()).or_insert_with(|| {
            log_info!("[ssdp] media server {usn} at {location}");
            Found {
                usn,
                location,
                server: header(&text, "server").map(|s| s.to_string()),
                from: from.to_string(),
            }
        });
    }

    out.listened_ms = started.elapsed().as_millis() as u64;
    out.other_service_types = other_types;
    out.servers = by_usn.into_values().collect();
    // Stable order, so a client rendering the list does not see it shuffle.
    out.servers.sort_by(|a, b| a.usn.cmp(&b.usn));

    if out.servers.is_empty() {
        log_info!(
            "[ssdp] no media servers: {}",
            out.empty_explanation().unwrap_or_default()
        );
    }
    out
}

/// Whether a `LOCATION` points back at the host the datagram came from.
///
/// **A hostname is refused, not resolved.** Resolving one would reintroduce the
/// whole problem: the sender chooses the name, and DNS or a hosts file decides
/// where it goes. UPnP 1.0 requires `LOCATION` to carry an IP address for
/// exactly this reason — the protocol runs on networks where name resolution
/// may not exist — so refusing a name costs nothing real.
///
/// Comparison is on the parsed address, never the string, so the textual
/// aliases do not matter: `localhost`, a trailing dot and `127.0.0.001` all
/// fail to parse and are refused, and an IPv4-mapped IPv6 literal is unmapped
/// before comparison rather than being allowed to differ textually from the
/// same address written the ordinary way.
fn location_is_the_sender(location: &Url, from: &SocketAddr) -> bool {
    let Ok(advertised) = location.host.parse::<IpAddr>() else {
        return false;
    };
    canonical(advertised) == canonical(from.ip())
}

/// Unmap IPv4-in-IPv6 so `::ffff:127.0.0.1` and `127.0.0.1` compare equal.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        v4 => v4,
    }
}

/// Case-insensitive header lookup on an SSDP datagram.
///
/// SSDP is HTTP-shaped but not HTTP: responders vary on line endings, so this
/// splits on `\n` and trims rather than requiring `\r\n`.
fn header<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    text.split('\n').skip(1).find_map(|line| {
        let (k, v) = line.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

#[cfg(test)]
mod tests {
    //! **No test in this module opens a socket.**
    //!
    //! `FOLLOW-UPS.md` records a unit test in a sibling crate that multicast
    //! `coyote.local` onto the real LAN on every `cargo test`, from a process
    //! that had already exited. This module is the one in this crate that would
    //! most obviously repeat that mistake — its whole job is to spray datagrams
    //! at a multicast group — so the invariant is written here rather than left
    //! to be noticed: everything below is a pure function over a captured
    //! datagram. [`discover`] itself is exercised by hand and by the integration
    //! test, never by `cargo test`.

    use super::*;

    /// A real UMS reply, reformatted only to fit. The shape is what matters:
    /// mixed-case header names, and `\r\n`.
    const UMS_REPLY: &str = "HTTP/1.1 200 OK\r\n\
        CACHE-CONTROL: max-age=1800\r\n\
        DATE: Tue, 29 Jul 2026 07:41:00 GMT\r\n\
        EXT: \r\n\
        LOCATION: http://192.168.0.4:5001/description/fetch\r\n\
        SERVER: Windows_NT/10.0 UPnP/1.0 UMS/14.10.0\r\n\
        ST: urn:schemas-upnp-org:device:MediaServer:1\r\n\
        USN: uuid:06b1f0ee-1234-4321-abcd-0011223344ff::urn:schemas-upnp-org:device:MediaServer:1\r\n\r\n";

    #[test]
    fn reads_headers_case_insensitively() {
        assert_eq!(
            header(UMS_REPLY, "location"),
            Some("http://192.168.0.4:5001/description/fetch")
        );
        assert_eq!(header(UMS_REPLY, "ST"), Some(MEDIA_SERVER));
        assert!(header(UMS_REPLY, "nonesuch").is_none());
    }

    /// The status line is not a header. Skipping it is why `header` starts at
    /// line 1 — otherwise `"HTTP/1.1 200 OK"` would parse as a header named
    /// `HTTP/1.1 200 OK`… with no colon, so in practice this guards against a
    /// future responder whose status line contains one.
    #[test]
    fn the_status_line_is_not_treated_as_a_header() {
        let odd = "HTTP/1.1 200 OK: yes\r\nST: x\r\n\r\n";
        assert_eq!(header(odd, "HTTP/1.1 200 OK"), None);
        assert_eq!(header(odd, "st"), Some("x"));
    }

    #[test]
    fn tolerates_bare_lf_line_endings() {
        let lf = "HTTP/1.1 200 OK\nST: x\nLOCATION: http://h/d\n\n";
        assert_eq!(header(lf, "location"), Some("http://h/d"));
    }

    /// **A responder may only describe itself.**
    ///
    /// `LOCATION` is a URL chosen by anything on the network that can send a
    /// datagram, and the bridge fetches it on every discovery — which is every
    /// time a client opens the picker. Unchecked, that is a GET-forgery
    /// primitive against loopback and the LAN, with an oracle attached, because
    /// the outcome is rendered verbatim in `/dlna/index.json`.
    ///
    /// The datagram's source address is the one field a sender cannot choose:
    /// `M-SEARCH` replies are unicast to an ephemeral port, so a forged source
    /// would never have reached the socket.
    #[test]
    fn a_location_pointing_anywhere_but_the_sender_is_refused() {
        let sender: SocketAddr = "192.168.0.4:56213".parse().unwrap();
        let ok = |u: &str| location_is_the_sender(&Url::parse(u).unwrap(), &sender);

        assert!(ok("http://192.168.0.4:5001/desc"), "its own address, any port");

        for hostile in [
            // The two the review demonstrated.
            "http://127.0.0.1:8787/pair",
            "http://192.168.0.1/admin",
            // Link-local metadata, the classic SSRF target.
            "http://169.254.169.254/latest/meta-data/",
            // A name is refused rather than resolved: the sender picks the
            // name and something else decides where it points.
            "http://localhost:5001/desc",
            "http://my-nas.local/desc",
            // Textual aliases for loopback. None parses as an address, so none
            // reaches the comparison at all.
            "http://127.0.0.001/desc",
            "http://127.0.0.1./desc",
            // A different host that merely starts the same.
            "http://192.168.0.44/desc",
        ] {
            assert!(!ok(hostile), "should refuse {hostile}");
        }
    }

    /// The same address written as IPv4-mapped IPv6 is the same address.
    /// Compared parsed rather than textually, so this is allowed and the
    /// aliases above are not.
    #[test]
    fn an_ipv4_mapped_sender_matches_its_plain_form() {
        let mapped: SocketAddr = "[::ffff:192.168.0.4]:56213".parse().unwrap();
        assert!(location_is_the_sender(
            &Url::parse("http://192.168.0.4:5001/desc").unwrap(),
            &mapped
        ));

        let plain: SocketAddr = "192.168.0.4:56213".parse().unwrap();
        assert!(!location_is_the_sender(
            &Url::parse("http://192.168.0.5:5001/desc").unwrap(),
            &plain
        ));
    }

    /// An empty result must be able to say which of the three empties it is.
    #[test]
    fn an_empty_result_explains_which_empty_it_is() {
        // Nothing answered at all — says so, and does not blame the network.
        let mut d = Discovery {
            bound_to: Some("192.168.0.9:54321".into()),
            bound_to_specific_interface: true,
            searches_sent: 3,
            ..Default::default()
        };
        let msg = d.empty_explanation().unwrap();
        assert!(msg.contains("nothing on the network answered at all"), "{msg}");

        // Something answered, but nothing was a media server — names the UMS
        // allowlist, which is the likely cause and lives in a component we do
        // not own.
        d.replies_seen = 4;
        d.other_service_types = vec!["upnp:rootdevice".into()];
        let msg = d.empty_explanation().unwrap();
        assert!(msg.contains("allowlist"), "{msg}");
        assert!(msg.contains("upnp:rootdevice"), "{msg}");

        // A search that never left is not reported as a fact about the network.
        let stuck = Discovery {
            bound_to: Some("0.0.0.0:1".into()),
            searches_sent: 0,
            problems: vec!["send 1 failed: network unreachable".into()],
            ..Default::default()
        };
        let msg = stuck.empty_explanation().unwrap();
        assert!(msg.contains("No search left the machine"), "{msg}");

        // And a non-empty result explains nothing, because there is nothing to
        // explain.
        let ok = Discovery {
            servers: vec![Found {
                usn: "uuid:x".into(),
                location: Url::parse("http://h/d").unwrap(),
                server: None,
                from: "192.168.0.4:1900".into(),
            }],
            ..Default::default()
        };
        assert!(ok.empty_explanation().is_none());
    }

    /// The fallback bind is called out by name, because it is the case that
    /// searches a virtual adapter and reports "nothing found".
    #[test]
    fn the_wildcard_bind_is_named_in_the_explanation() {
        let d = Discovery {
            bound_to: Some("0.0.0.0:54321".into()),
            bound_to_specific_interface: false,
            searches_sent: 3,
            ..Default::default()
        };
        let msg = d.empty_explanation().unwrap();
        assert!(msg.contains("virtual adapters"), "{msg}");
    }
}
