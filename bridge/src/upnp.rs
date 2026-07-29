//! UPnP: device descriptions, ContentDirectory `Browse`, and DIDL-Lite.
//!
//! Everything in this module reads XML produced by a media server we did not
//! write. It is the only module that touches `quick-xml`, which is deliberate —
//! `Cargo.toml` records why that dependency exists, and confining it here means
//! reversing that decision is a change to one file.
//!
//! ## The three steps
//!
//! 1. **Device description.** [`ssdp`](crate::ssdp) gives a `LOCATION`; fetching
//!    it yields XML naming the device and its services. We want one service:
//!    `urn:schemas-upnp-org:service:ContentDirectory:1`, and specifically its
//!    `<controlURL>`.
//! 2. **`Browse`.** A SOAP POST to that control URL, with `ObjectID` and
//!    `BrowseDirectChildren`. Paginated — a real library does not fit one
//!    response, and UMS caps a response regardless of what you ask for.
//! 3. **DIDL-Lite.** The `Browse` response carries the listing as *escaped XML
//!    inside an XML element*, so it is parsed twice. That is the protocol, not
//!    a mistake.
//!
//! ## Choosing a `<res>` is the part that decides whether video appears
//!
//! A server advertises the same item several times: the original file, a
//! transcode, sometimes a thumbnail, sometimes a stream over a protocol a
//! browser cannot speak at all. Taking the first one gives, in the handoff
//! document's words, "a video element that loads and shows nothing" — and that
//! failure is silent, which puts it squarely in `FOLLOW-UPS.md` §0b territory:
//! the blame lands on the file or the network.
//!
//! So [`choose_res`] is explicit about its ranking, and — more importantly —
//! [`Chosen`] carries the ones it *rejected* and why. When a video will not
//! play, the answer to "what else was on offer" is one field away instead of a
//! packet capture away.
//!
//! The ranking, in order:
//!
//! 1. **`http-get` only.** `rtsp-rtp-udp`, `internal` and the rest are not
//!    things a `<video>` element can open, and neither is the proxy.
//! 2. **A container the browser will actually decode.** Matroska, AVI and
//!    MPEG-2 are common DLNA offerings and are not playable in Safari, which is
//!    the browser this has to work in — the phone is iOS and Web Bluetooth
//!    means Bluefy, which is WebKit. Preferring `video/mp4` is not a taste.
//! 3. **Byte-range support.** `DLNA.ORG_OP`'s second digit is "server supports
//!    Range requests". **This is the acceptance condition of the whole task** —
//!    a resource without it cannot be scrubbed no matter how correct the proxy
//!    is, because there is nothing upstream to forward the range to.
//! 4. **Original over transcode.** `DLNA.ORG_CI=1` marks converted content.
//!    A transcode is usually on-demand, usually not seekable, and usually
//!    worse.
//!
//! ## Bounds
//!
//! `quick-xml` will happily parse a document with a million elements nested a
//! million deep. The limits that stop that are here, not in the library:
//! [`MAX_XML_BYTES`], [`MAX_DEPTH`], [`MAX_ITEMS`]. They are checked while
//! parsing rather than after, so an oversized document costs a bounded amount
//! of work and not just a bounded amount of output.

use std::collections::HashMap;

use quick_xml::events::Event;
use quick_xml::Reader;
use serde::Serialize;

use crate::httpc::{self, Url};
use crate::{log_debug, log_warn};

/// The service we need on a media server.
pub const CONTENT_DIRECTORY: &str = "urn:schemas-upnp-org:service:ContentDirectory:1";

/// The root object every ContentDirectory has.
pub const ROOT_OBJECT: &str = "0";

/// Cap on a device description or `Browse` response.
///
/// A `Browse` for 200 items with long titles is tens of kilobytes; 4 MB is far
/// past anything genuine. The cap matters because the URL being fetched came
/// from a UDP datagram any host on the network can send.
pub const MAX_XML_BYTES: usize = 4 * 1024 * 1024;

/// Maximum element nesting. DIDL-Lite is three or four deep in practice.
const MAX_DEPTH: usize = 64;

/// Maximum items or containers taken from one `Browse` response, whatever the
/// server claims it returned.
const MAX_ITEMS: usize = 2_000;

/// How many children to ask for per `Browse`. Servers may return fewer.
pub const PAGE_SIZE: u32 = 200;

// ---------------------------------------------------------------------------
// Device description
// ---------------------------------------------------------------------------

/// What we keep from a device description.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    /// `UDN` — `uuid:…`. The stable identity, and the handle a client holds.
    pub udn: String,
    pub friendly_name: String,
    /// `SERVER`-ish free text from the description, when present.
    pub model: Option<String>,
    /// Where `Browse` is POSTed.
    #[serde(serialize_with = "url_str")]
    pub control_url: Url,
    /// Where the description itself came from. Kept because every `res` URL
    /// this device later advertises is checked against this host — see
    /// [`crate::dlna`].
    #[serde(serialize_with = "url_str")]
    pub description_url: Url,
}

fn url_str<S: serde::Serializer>(u: &Url, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&u.to_string())
}

/// Fetch and parse a device description.
pub async fn describe(location: &Url) -> Result<Device, String> {
    let (head, body) = httpc::fetch_bounded("GET", location, &[], None, MAX_XML_BYTES)
        .await
        .map_err(|e| format!("could not fetch the device description at {location}: {e}"))?;
    if head.status != 200 {
        return Err(format!(
            "{location} answered {} to a description fetch",
            head.status
        ));
    }
    parse_description(&body, location)
}

/// Parse a device description into the one service we need.
///
/// A description may nest devices (`<deviceList>`), and the ContentDirectory
/// may hang off an embedded device rather than the root. Rather than model the
/// tree, this walks it and remembers the *most recent* `UDN` and
/// `friendlyName`, attaching them to the ContentDirectory when it is found.
/// That is correct for the shape servers actually emit — the service lives
/// inside the device element that owns it — and it degrades to "the root
/// device's name" rather than to nothing.
pub fn parse_description(xml: &[u8], base: &Url) -> Result<Device, String> {
    if xml.len() > MAX_XML_BYTES {
        return Err("device description is too large".into());
    }
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);

    let mut buf = Vec::new();
    let mut path: Vec<String> = Vec::new();

    // Most recent values seen, per the doc comment above.
    let mut udn = String::new();
    let mut friendly = String::new();
    let mut model: Option<String> = None;

    // Accumulated while inside a `<service>`.
    let mut service_type = String::new();
    let mut control_url = String::new();
    let mut found: Option<(String, String, Option<String>, String)> = None;

    // `<URLBase>` overrides the description URL as the base for relative
    // references. Rare, and specified, and getting it wrong sends `Browse` to
    // the wrong port.
    let mut url_base = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Err(e) => return Err(format!("malformed device description: {e}")),
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) => {
                if path.len() >= MAX_DEPTH {
                    return Err("device description is nested too deeply".into());
                }
                let name = local_name(e.name().as_ref());
                if name == "service" {
                    service_type.clear();
                    control_url.clear();
                }
                path.push(name);
            }
            Ok(Event::End(e)) => {
                let name = local_name(e.name().as_ref());
                if name == "service"
                    && service_type.eq_ignore_ascii_case(CONTENT_DIRECTORY)
                    && !control_url.is_empty()
                    && found.is_none()
                {
                    found = Some((
                        udn.clone(),
                        friendly.clone(),
                        model.clone(),
                        control_url.clone(),
                    ));
                }
                path.pop();
            }
            Ok(Event::Text(t)) => {
                let Some(current) = path.last() else { continue };
                let value = match t.unescape() {
                    Ok(v) => v.trim().to_string(),
                    Err(_) => continue,
                };
                if value.is_empty() {
                    continue;
                }
                match current.as_str() {
                    "UDN" => udn = value,
                    "friendlyName" => friendly = value,
                    "modelName" | "modelDescription" if model.is_none() => model = Some(value),
                    "URLBase" => url_base = value,
                    "serviceType" => service_type = value,
                    "controlURL" => control_url = value,
                    _ => {}
                }
            }
            _ => {}
        }
        buf.clear();
    }

    let Some((udn, friendly, model, control)) = found else {
        return Err(format!(
            "{base} is a device description with no {CONTENT_DIRECTORY} service — it answered the \
             media-server search but does not offer browsing"
        ));
    };

    // Resolve the control URL. `URLBase` wins if the server gave one.
    let resolve_base = if url_base.is_empty() {
        base.clone()
    } else {
        Url::parse(&url_base).unwrap_or_else(|| base.clone())
    };
    let control_url = resolve_base
        .resolve(&control)
        .ok_or_else(|| format!("control URL {control:?} could not be resolved against {resolve_base}"))?;

    // A device does not get to point its own control URL at another host. It
    // would turn "browse my media server" into "make the bridge POST arbitrary
    // XML wherever I say", from a device that only had to answer a UDP search.
    if control_url.host != base.host || control_url.port != base.port {
        return Err(format!(
            "{base} advertised a control URL on a different host ({}); refused",
            control_url.authority()
        ));
    }

    Ok(Device {
        udn: if udn.is_empty() {
            base.to_string()
        } else {
            udn
        },
        friendly_name: if friendly.is_empty() {
            base.host.clone()
        } else {
            friendly
        },
        model,
        control_url,
        description_url: base.clone(),
    })
}

/// The last path segment of a possibly-namespaced element name.
///
/// `dc:title` and `title` must compare equal; so must `upnp:class` and `class`.
/// Namespace *prefixes* are not fixed by the spec — only the URIs they bind to
/// are — so matching on the prefix would be wrong even though every real server
/// uses `dc` and `upnp`.
fn local_name(raw: &[u8]) -> String {
    let s = String::from_utf8_lossy(raw);
    match s.rsplit_once(':') {
        Some((_, local)) => local.to_string(),
        None => s.into_owned(),
    }
}

// ---------------------------------------------------------------------------
// Browse
// ---------------------------------------------------------------------------

/// One directory level, as `Browse` returned it.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Listing {
    pub containers: Vec<Container>,
    pub items: Vec<Item>,
    /// What the server said it returned in this response.
    pub number_returned: u32,
    /// What the server said the total is. Servers lie about this — UMS reports
    /// 0 for some containers — so a caller must not use it as the loop
    /// condition. Kept because it is useful when it is right.
    pub total_matches: u32,
}

/// A browsable folder.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Container {
    pub id: String,
    pub title: String,
    /// `childCount`, when advertised. Advisory.
    pub child_count: Option<u32>,
}

/// A playable thing.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: String,
    /// `dc:title` — a real title, not a filename scraped from a path segment.
    /// The handoff document flags this as the better input to script matching.
    pub title: String,
    /// `upnp:class`, e.g. `object.item.videoItem`.
    pub class: String,
    /// Every representation the server offered, in the order it offered them.
    pub resources: Vec<Res>,
}

/// One `<res>` element: a URL plus the terms on which it is served.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Res {
    pub url: String,
    /// Raw `protocolInfo`, kept verbatim so a diagnosis is not limited to the
    /// fields this code happened to parse.
    pub protocol_info: String,
    pub size: Option<u64>,
    /// `H:MM:SS.mmm` as the server wrote it.
    pub duration: Option<String>,
    pub resolution: Option<String>,
}

impl Res {
    /// `protocolInfo` is four colon-separated fields:
    /// `protocol:network:contentFormat:additionalInfo`.
    fn field(&self, n: usize) -> Option<&str> {
        self.protocol_info.split(':').nth(n)
    }

    /// The transport. Only `http-get` is fetchable by this bridge or playable
    /// by a browser.
    pub fn protocol(&self) -> &str {
        self.field(0).unwrap_or("")
    }

    /// The MIME type, e.g. `video/mp4`.
    pub fn mime(&self) -> &str {
        self.field(2).unwrap_or("")
    }

    /// A `DLNA.ORG_*` flag from the fourth field.
    fn dlna_flag(&self, key: &str) -> Option<&str> {
        self.field(3)?.split(';').find_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            k.trim().eq_ignore_ascii_case(key).then(|| v.trim())
        })
    }

    /// Whether the server says it honours `Range`.
    ///
    /// `DLNA.ORG_OP` is two digits: time-seek, then byte-seek. The second is
    /// the one that decides whether scrubbing can work at all. Absent means
    /// "unstated", which is not the same as "no" — plenty of servers omit it
    /// and support ranges anyway — so this is a tri-state and the ranking
    /// treats unstated as worse than stated-yes and better than stated-no.
    pub fn byte_range(&self) -> Option<bool> {
        let op = self.dlna_flag("DLNA.ORG_OP")?;
        op.chars().nth(1).map(|c| c == '1')
    }

    /// `DLNA.ORG_CI=1` means the server converted this from the original.
    pub fn is_transcode(&self) -> bool {
        self.dlna_flag("DLNA.ORG_CI") == Some("1")
    }
}

/// Ask a ContentDirectory for the direct children of one object.
pub async fn browse(
    control: &Url,
    object_id: &str,
    start: u32,
    count: u32,
) -> Result<Listing, String> {
    // `ObjectID` is server-supplied on every path except the root, but a client
    // can send one back, so it is escaped rather than interpolated raw. An
    // unescaped `"` here would let a crafted id rewrite the SOAP body.
    let envelope = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
<s:Body>
<u:Browse xmlns:u="{CONTENT_DIRECTORY}">
<ObjectID>{}</ObjectID>
<BrowseFlag>BrowseDirectChildren</BrowseFlag>
<Filter>*</Filter>
<StartingIndex>{start}</StartingIndex>
<RequestedCount>{count}</RequestedCount>
<SortCriteria></SortCriteria>
</u:Browse>
</s:Body>
</s:Envelope>"#,
        xml_escape(object_id)
    );

    let soap_action = format!("\"{CONTENT_DIRECTORY}#Browse\"");
    let (head, body) = httpc::fetch_bounded(
        "POST",
        control,
        &[
            ("Content-Type", "text/xml; charset=\"utf-8\""),
            ("SOAPAction", &soap_action),
        ],
        Some(envelope.as_bytes()),
        MAX_XML_BYTES,
    )
    .await
    .map_err(|e| format!("Browse of {object_id:?} at {control} failed: {e}"))?;

    if head.status != 200 {
        // A SOAP fault body is XML and carries a UPnP error code. Surfacing the
        // raw beginning of it is worth more than "500": `701` is "no such
        // object", `501` is "action failed", and a server refusing this host
        // tends to answer 500 with something legible.
        let hint = String::from_utf8_lossy(&body[..body.len().min(400)]).replace('\n', " ");
        return Err(format!(
            "Browse of {object_id:?} answered {}: {hint}",
            head.status
        ));
    }

    let didl = extract_soap_result(&body)
        .ok_or_else(|| format!("Browse of {object_id:?} returned no <Result> element"))?;
    let mut listing = parse_didl(didl.as_bytes())?;
    listing.number_returned = soap_number(&body, "NumberReturned").unwrap_or(
        (listing.items.len() + listing.containers.len()) as u32,
    );
    listing.total_matches = soap_number(&body, "TotalMatches").unwrap_or(0);
    log_debug!(
        "[upnp] browse {object_id:?}[{start}..] -> {} containers, {} items",
        listing.containers.len(),
        listing.items.len()
    );
    Ok(listing)
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// Pull the DIDL-Lite document out of the SOAP envelope's `<Result>`.
///
/// The listing arrives as escaped XML *inside* an XML text node, so this is the
/// first of two parses. `unescape` handles both entity-escaped and CDATA forms,
/// which servers split between roughly evenly.
fn extract_soap_result(xml: &[u8]) -> Option<String> {
    read_element_text(xml, "Result")
}

fn soap_number(xml: &[u8], name: &str) -> Option<u32> {
    read_element_text(xml, name)?.trim().parse().ok()
}

/// Concatenated text of the first element with this local name.
fn read_element_text(xml: &[u8], want: &str) -> Option<String> {
    let mut reader = Reader::from_reader(xml);
    // Not `trim_text`: the DIDL payload's own whitespace is significant enough
    // that trimming inside it is a needless risk, and this only reads the one
    // element.
    let mut buf = Vec::new();
    let mut depth = 0usize;
    let mut inside = false;
    let mut out = String::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Err(_) | Ok(Event::Eof) => break,
            Ok(Event::Start(e)) => {
                depth += 1;
                if depth > MAX_DEPTH {
                    break;
                }
                if !inside && local_name(e.name().as_ref()) == want {
                    inside = true;
                }
            }
            Ok(Event::End(e)) => {
                depth = depth.saturating_sub(1);
                if inside && local_name(e.name().as_ref()) == want {
                    return Some(out);
                }
            }
            Ok(Event::Text(t)) if inside => {
                if let Ok(v) = t.unescape() {
                    out.push_str(&v);
                }
            }
            Ok(Event::CData(t)) if inside => {
                out.push_str(&String::from_utf8_lossy(t.as_ref()));
            }
            _ => {}
        }
        buf.clear();
    }
    if inside {
        Some(out)
    } else {
        None
    }
}

/// Parse a DIDL-Lite document into containers and items.
pub fn parse_didl(xml: &[u8]) -> Result<Listing, String> {
    if xml.len() > MAX_XML_BYTES {
        return Err("DIDL response is too large".into());
    }
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();

    let mut listing = Listing::default();
    let mut depth = 0usize;
    let mut truncated = false;

    // What we are currently inside, if anything.
    enum In {
        Nothing,
        Item(Item),
        Container(Container),
    }
    let mut current = In::Nothing;
    // The `<res>` being accumulated, and the element whose text we are in.
    let mut res: Option<Res> = None;
    let mut text_target = String::new();

    loop {
        let event = match reader.read_event_into(&mut buf) {
            Ok(e) => e,
            Err(e) => {
                // A truncated or malformed listing yields what parsed cleanly
                // up to the break rather than nothing. A partial library is
                // more useful than an error, and the log names the cause so
                // "some files are missing" is not a mystery.
                log_warn!("[upnp] DIDL parse stopped early: {e}");
                break;
            }
        };
        match event {
            Event::Eof => break,
            // `Empty` is a self-closing element: `<res …/>`. It carries the
            // same attributes as a `Start` and is followed by no `End`, so it
            // is opened and closed here. Folding it into the `Start` arm would
            // leave `depth` and `current` permanently unbalanced — the classic
            // way a hand-written XML consumer loses every item after the first
            // self-closing tag.
            Event::Empty(e) => {
                let name = local_name(e.name().as_ref());
                match name.as_str() {
                    // A `<res/>` with no text has no URL, so there is nothing
                    // to record. Containers and items still count.
                    "container" => {
                        let c = Container {
                            id: attr(&e, "id").unwrap_or_default(),
                            title: String::new(),
                            child_count: attr(&e, "childCount").and_then(|v| v.parse().ok()),
                        };
                        if listing.items.len() + listing.containers.len() >= MAX_ITEMS {
                            truncated = true;
                        } else if !c.id.is_empty() {
                            listing.containers.push(c);
                        }
                    }
                    "item" => {
                        let i = Item {
                            id: attr(&e, "id").unwrap_or_default(),
                            title: String::new(),
                            class: String::new(),
                            resources: Vec::new(),
                        };
                        if listing.items.len() + listing.containers.len() >= MAX_ITEMS {
                            truncated = true;
                        } else if !i.id.is_empty() {
                            listing.items.push(i);
                        }
                    }
                    _ => {}
                }
                text_target.clear();
            }
            Event::Start(e) => {
                let name = local_name(e.name().as_ref());
                depth += 1;
                if depth > MAX_DEPTH {
                    return Err("DIDL is nested too deeply".into());
                }
                match name.as_str() {
                    "item" => {
                        current = In::Item(Item {
                            id: attr(&e, "id").unwrap_or_default(),
                            title: String::new(),
                            class: String::new(),
                            resources: Vec::new(),
                        });
                    }
                    "container" => {
                        current = In::Container(Container {
                            id: attr(&e, "id").unwrap_or_default(),
                            title: String::new(),
                            child_count: attr(&e, "childCount").and_then(|v| v.parse().ok()),
                        });
                    }
                    "res" => {
                        res = Some(Res {
                            url: String::new(),
                            protocol_info: attr(&e, "protocolInfo").unwrap_or_default(),
                            size: attr(&e, "size").and_then(|v| v.parse().ok()),
                            duration: attr(&e, "duration"),
                            resolution: attr(&e, "resolution"),
                        });
                    }
                    _ => {}
                }
                text_target = name;
            }
            Event::Text(t) => {
                let Ok(value) = t.unescape() else { continue };
                let value = value.trim();
                if value.is_empty() {
                    continue;
                }
                match text_target.as_str() {
                    "res" => {
                        if let Some(r) = res.as_mut() {
                            r.url.push_str(value);
                        }
                    }
                    "title" => match &mut current {
                        In::Item(i) if i.title.is_empty() => i.title = value.to_string(),
                        In::Container(c) if c.title.is_empty() => c.title = value.to_string(),
                        _ => {}
                    },
                    "class" => {
                        if let In::Item(i) = &mut current {
                            i.class = value.to_string();
                        }
                    }
                    _ => {}
                }
            }
            Event::End(e) => {
                depth = depth.saturating_sub(1);
                let name = local_name(e.name().as_ref());
                match name.as_str() {
                    "res" => {
                        if let (Some(r), In::Item(i)) = (res.take(), &mut current) {
                            if !r.url.is_empty() {
                                i.resources.push(r);
                            }
                        }
                    }
                    "item" => {
                        if let In::Item(i) = std::mem::replace(&mut current, In::Nothing) {
                            if listing.items.len() + listing.containers.len() >= MAX_ITEMS {
                                truncated = true;
                            } else if !i.id.is_empty() {
                                listing.items.push(i);
                            }
                        }
                    }
                    "container" => {
                        if let In::Container(c) = std::mem::replace(&mut current, In::Nothing) {
                            if listing.items.len() + listing.containers.len() >= MAX_ITEMS {
                                truncated = true;
                            } else if !c.id.is_empty() {
                                listing.containers.push(c);
                            }
                        }
                    }
                    _ => {}
                }
                text_target.clear();
            }
            _ => {}
        }
        buf.clear();
    }

    if truncated {
        log_warn!("[upnp] DIDL listing truncated at {MAX_ITEMS} entries");
    }
    Ok(listing)
}

/// One attribute of a start tag, unescaped.
fn attr(e: &quick_xml::events::BytesStart, name: &str) -> Option<String> {
    for a in e.attributes().flatten() {
        if local_name(a.key.as_ref()) == name {
            return a.unescape_value().ok().map(|v| v.into_owned());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Choosing a representation
// ---------------------------------------------------------------------------

/// The outcome of picking a `<res>`, including what was passed over.
///
/// The rejections are the point. "It loads and shows nothing" is otherwise an
/// unfalsifiable complaint about the video; with this, the answer is that the
/// only offer was Matroska, or that the chosen resource says it does not
/// support byte ranges.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Chosen {
    pub res: Res,
    /// Why this one, in one line, for a UI or a log.
    pub reason: String,
    /// The rest, each with the reason it lost.
    pub rejected: Vec<(String, String)>,
    /// `false` when the server explicitly said it does not honour `Range`.
    /// Scrubbing will not work and the proxy is not at fault.
    pub seekable: bool,
}

/// MIME types a WebKit `<video>`/`<audio>` element will decode.
///
/// The phone is iOS, because Web Bluetooth on iOS means Bluefy, which is a
/// WebKit wrapper and therefore uses the system engine. So this list is what
/// **WebKit** plays, not what Chrome plays — and the two differ in a direction
/// that matters, since a container Chrome decodes and WebKit does not gives a
/// video element that loads and shows nothing on the one device this is for.
///
/// # Where this list comes from
///
/// **Documentation, not observation.** Nothing here has been played in WebKit;
/// this is the one load-bearing claim in the module that rests on reading
/// rather than running, and it is written down so the next person can check it
/// instead of inheriting it.
///
/// - **H.264 and HEVC in MP4 are the reliable pair.** Apple's *Creating Video
///   for Safari on iPhone* is the long-standing statement of this, and it is
///   why [`score`] ranks `video/mp4` above everything else that is merely
///   accepted.
///   <https://developer.apple.com/library/archive/documentation/AppleApplications/Reference/SafariWebContent/CreatingVideoforSafarioniPhone/CreatingVideoforSafarioniPhone.html>
/// - **WebM is supported, but recently, and with a caveat.** Safari on macOS
///   has had WebM with VP8 and VP9 since 14.1; **iOS and iPadOS only gained it
///   in 17.4** — before that it was VP8 in WebRTC only. So `video/webm` here
///   assumes a phone on iOS 17.4 or later.
///   <https://webkit.org/blog/15063/webkit-features-in-safari-17-4/>
///   The caveat: VP9 is decoded for 4:2:0 and 4:2:2 chroma subsampling only, so
///   a 4:4:4 file plays in Chromium and Firefox and not here. A DLNA server is
///   unlikely to be serving 4:4:4 VP9, which is why this is a note rather than
///   an exclusion.
/// - **Matroska, AVI and MPEG-2 are excluded**, and they are the common DLNA
///   offerings that a naive "take the first `<res>`" would pick. Universal
///   Media Server listed `video/x-matroska` *before* `video/mp4` in the fixture
///   this module is tested against.
///
/// # If this list is wrong
///
/// The symptom is a video element that loads and shows nothing, with no error —
/// the silent failure this module is arranged around. [`Chosen::rejected`]
/// carries what was passed over and why, so the first question ("what else was
/// on offer?") is answerable from `/dlna/browse.json` rather than from a packet
/// capture. Correct the list here; do not special-case at the call site.
fn browser_playable(mime: &str) -> bool {
    matches!(
        mime.to_ascii_lowercase().as_str(),
        "video/mp4"
            | "video/quicktime"
            | "video/webm"
            | "audio/mpeg"
            | "audio/mp4"
            | "audio/aac"
            | "audio/x-m4a"
            | "audio/wav"
            | "audio/x-wav"
            | "image/jpeg"
            | "image/png"
            | "image/webp"
    )
}

/// Pick the resource most likely to play and to scrub, or say why none will.
pub fn choose_res(item: &Item) -> Result<Chosen, String> {
    if item.resources.is_empty() {
        return Err(format!("{:?} advertises no <res> at all", item.title));
    }

    let mut rejected: Vec<(String, String)> = Vec::new();
    let mut candidates: Vec<&Res> = Vec::new();

    for r in &item.resources {
        if r.protocol() != "http-get" {
            rejected.push((
                r.url.clone(),
                format!(
                    "transport is {:?}, which neither the proxy nor a browser can open",
                    r.protocol()
                ),
            ));
            continue;
        }
        if !browser_playable(r.mime()) {
            rejected.push((
                r.url.clone(),
                format!("{:?} is not a container WebKit decodes", r.mime()),
            ));
            continue;
        }
        candidates.push(r);
    }

    let Some(best) = candidates
        .iter()
        .copied()
        .max_by_key(|r| score(r))
        .cloned()
    else {
        return Err(format!(
            "no playable resource for {:?}. Offered: {}",
            item.title,
            item.resources
                .iter()
                .map(|r| format!("{} ({})", r.mime(), r.protocol()))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    };

    for r in candidates {
        if r.url != best.url {
            rejected.push((
                r.url.clone(),
                format!(
                    "playable, but ranked below the chosen one (transcode: {}, byte-range: {:?})",
                    r.is_transcode(),
                    r.byte_range()
                ),
            ));
        }
    }

    let seekable = best.byte_range() != Some(false);
    let reason = format!(
        "{} ({}{}{})",
        best.mime(),
        match best.byte_range() {
            Some(true) => "byte ranges advertised",
            Some(false) => "server says it does NOT honour byte ranges — scrubbing will not work",
            None => "byte-range support unstated",
        },
        if best.is_transcode() {
            ", transcoded"
        } else {
            ""
        },
        match best.size {
            Some(n) => format!(", {n} bytes"),
            None => String::new(),
        }
    );

    Ok(Chosen {
        res: best,
        reason,
        rejected,
        seekable,
    })
}

/// Ranking key. Higher is better; ordering is by tuple, so earlier fields
/// dominate — which is the ranking documented at the top of this module.
fn score(r: &Res) -> (u8, u8, u8, u64) {
    (
        // Byte ranges: stated-yes beats unstated beats stated-no.
        match r.byte_range() {
            Some(true) => 2,
            None => 1,
            Some(false) => 0,
        },
        // Original beats transcode.
        u8::from(!r.is_transcode()),
        // mp4 over everything else playable, because it is the one WebKit is
        // certain about.
        u8::from(r.mime().eq_ignore_ascii_case("video/mp4")),
        // Bigger is usually the original rather than a downscale. Last, so it
        // only breaks ties.
        r.size.unwrap_or(0),
    )
}

/// Index a listing's items by id, for the lookups a proxy request needs.
pub fn by_id(listing: &Listing) -> HashMap<&str, &Item> {
    listing.items.iter().map(|i| (i.id.as_str(), i)).collect()
}

#[cfg(test)]
mod tests {
    //! Nothing here opens a socket. Every test is a pure function over a
    //! captured or hand-built document — see the same note in
    //! [`crate::ssdp`]'s tests for why that is stated rather than assumed.

    use super::*;

    const UMS_DESCRIPTION: &[u8] = br#"<?xml version="1.0"?>
<root xmlns="urn:schemas-upnp-org:device-1-0">
  <specVersion><major>1</major><minor>0</minor></specVersion>
  <device>
    <deviceType>urn:schemas-upnp-org:device:MediaServer:1</deviceType>
    <friendlyName>Universal Media Server</friendlyName>
    <modelName>UMS</modelName>
    <UDN>uuid:06b1f0ee-1234-4321-abcd-0011223344ff</UDN>
    <serviceList>
      <service>
        <serviceType>urn:schemas-upnp-org:service:ConnectionManager:1</serviceType>
        <controlURL>/upnp/control/connection_manager</controlURL>
      </service>
      <service>
        <serviceType>urn:schemas-upnp-org:service:ContentDirectory:1</serviceType>
        <controlURL>/upnp/control/content_directory</controlURL>
      </service>
    </serviceList>
  </device>
</root>"#;

    #[test]
    fn finds_the_content_directory_control_url() {
        let base = Url::parse("http://192.168.0.4:5001/description/fetch").unwrap();
        let d = parse_description(UMS_DESCRIPTION, &base).unwrap();
        assert_eq!(d.udn, "uuid:06b1f0ee-1234-4321-abcd-0011223344ff");
        assert_eq!(d.friendly_name, "Universal Media Server");
        assert_eq!(
            d.control_url.to_string(),
            "http://192.168.0.4:5001/upnp/control/content_directory"
        );
    }

    /// The ConnectionManager comes first in the document. Taking the first
    /// `<controlURL>` would POST `Browse` to it and get a fault back.
    #[test]
    fn does_not_take_the_first_service_it_sees() {
        let base = Url::parse("http://192.168.0.4:5001/description/fetch").unwrap();
        let d = parse_description(UMS_DESCRIPTION, &base).unwrap();
        assert!(!d.control_url.path_and_query.contains("connection_manager"));
    }

    /// A device that answered the media-server search but offers no browsing
    /// gets a message that says that, rather than an empty listing later.
    #[test]
    fn a_description_without_content_directory_says_so() {
        let base = Url::parse("http://h/d").unwrap();
        let xml = br#"<root><device><UDN>uuid:x</UDN><serviceList><service>
            <serviceType>urn:schemas-upnp-org:service:ConnectionManager:1</serviceType>
            <controlURL>/c</controlURL></service></serviceList></device></root>"#;
        let err = parse_description(xml, &base).unwrap_err();
        assert!(err.contains("no urn:schemas-upnp-org:service:ContentDirectory:1"), "{err}");
    }

    /// A device may not redirect its own control URL to another host. It only
    /// had to answer a UDP datagram to get here.
    #[test]
    fn a_control_url_on_another_host_is_refused() {
        let base = Url::parse("http://192.168.0.4:5001/d").unwrap();
        let xml = br#"<root><device><UDN>uuid:x</UDN><serviceList><service>
            <serviceType>urn:schemas-upnp-org:service:ContentDirectory:1</serviceType>
            <controlURL>http://10.0.0.1:8080/admin</controlURL>
            </service></serviceList></device></root>"#;
        let err = parse_description(xml, &base).unwrap_err();
        assert!(err.contains("different host"), "{err}");
    }

    #[test]
    fn url_base_overrides_the_description_url() {
        let base = Url::parse("http://192.168.0.4:5001/desc").unwrap();
        let xml = br#"<root><URLBase>http://192.168.0.4:5001/base/</URLBase>
            <device><UDN>uuid:x</UDN><serviceList><service>
            <serviceType>urn:schemas-upnp-org:service:ContentDirectory:1</serviceType>
            <controlURL>ctrl</controlURL></service></serviceList></device></root>"#;
        let d = parse_description(xml, &base).unwrap();
        assert_eq!(d.control_url.path_and_query, "/base/ctrl");
    }

    const BROWSE_RESPONSE: &[u8] = br#"<?xml version="1.0"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
<s:Body><u:BrowseResponse xmlns:u="urn:schemas-upnp-org:service:ContentDirectory:1">
<Result>&lt;DIDL-Lite xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:upnp="urn:schemas-upnp-org:metadata-1-0/upnp/"&gt;&lt;container id="1$7" parentID="1" childCount="12"&gt;&lt;dc:title&gt;Cock Hero&lt;/dc:title&gt;&lt;/container&gt;&lt;item id="1$7$253" parentID="1$7"&gt;&lt;dc:title&gt;Cock Hero Island 5 Episode I&lt;/dc:title&gt;&lt;upnp:class&gt;object.item.videoItem&lt;/upnp:class&gt;&lt;res protocolInfo="http-get:*:video/x-matroska:DLNA.ORG_OP=01" size="9000000000"&gt;http://192.168.0.4:5001/ums/media/06b1f0ee/253/x.mkv&lt;/res&gt;&lt;res protocolInfo="http-get:*:video/mp4:DLNA.ORG_OP=01;DLNA.ORG_CI=0" size="8000000000" duration="1:23:45.000" resolution="3840x1920"&gt;http://192.168.0.4:5001/ums/media/06b1f0ee/253/Cock-Hero-Island-5-Episode-I.mp4&lt;/res&gt;&lt;/item&gt;&lt;/DIDL-Lite&gt;</Result>
<NumberReturned>2</NumberReturned><TotalMatches>2</TotalMatches>
</u:BrowseResponse></s:Body></s:Envelope>"#;

    #[test]
    fn unwraps_the_doubly_encoded_didl() {
        let didl = extract_soap_result(BROWSE_RESPONSE).unwrap();
        assert!(didl.starts_with("<DIDL-Lite"), "{didl}");
        assert_eq!(soap_number(BROWSE_RESPONSE, "NumberReturned"), Some(2));
    }

    #[test]
    fn parses_containers_and_items_with_real_titles() {
        let didl = extract_soap_result(BROWSE_RESPONSE).unwrap();
        let listing = parse_didl(didl.as_bytes()).unwrap();

        assert_eq!(listing.containers.len(), 1);
        assert_eq!(listing.containers[0].id, "1$7");
        assert_eq!(listing.containers[0].title, "Cock Hero");
        assert_eq!(listing.containers[0].child_count, Some(12));

        assert_eq!(listing.items.len(), 1);
        let item = &listing.items[0];
        assert_eq!(item.id, "1$7$253");
        // The whole reason `dc:title` is worth having: spaces, not the dashes
        // UMS puts in the path.
        assert_eq!(item.title, "Cock Hero Island 5 Episode I");
        assert_eq!(item.class, "object.item.videoItem");
        assert_eq!(item.resources.len(), 2);
    }

    /// A CDATA-wrapped `Result` is the other half of what servers emit.
    #[test]
    fn a_cdata_wrapped_result_parses_too() {
        let xml = br#"<s:Envelope><s:Body><Result><![CDATA[<DIDL-Lite><item id="9">
            <dc:title>Nine</dc:title></item></DIDL-Lite>]]></Result></s:Body></s:Envelope>"#;
        let didl = extract_soap_result(xml).unwrap();
        let listing = parse_didl(didl.as_bytes()).unwrap();
        assert_eq!(listing.items[0].title, "Nine");
    }

    /// **The defect the handoff document warns about.** Matroska is listed
    /// first and is what a naive "take `resources[0]`" picks; Safari will not
    /// decode it and the video element loads and shows nothing.
    #[test]
    fn does_not_take_the_first_res() {
        let didl = extract_soap_result(BROWSE_RESPONSE).unwrap();
        let listing = parse_didl(didl.as_bytes()).unwrap();
        let chosen = choose_res(&listing.items[0]).unwrap();

        assert!(chosen.res.url.ends_with(".mp4"), "{}", chosen.res.url);
        assert_eq!(chosen.res.mime(), "video/mp4");
        assert!(chosen.seekable);
        assert!(
            chosen
                .rejected
                .iter()
                .any(|(url, why)| url.ends_with(".mkv") && why.contains("WebKit")),
            "the rejection must name why, not just that: {:?}",
            chosen.rejected
        );
    }

    #[test]
    fn parses_protocol_info_fields() {
        let r = Res {
            url: "http://h/x".into(),
            protocol_info: "http-get:*:video/mp4:DLNA.ORG_PN=AVC_MP4;DLNA.ORG_OP=01;DLNA.ORG_CI=1"
                .into(),
            size: None,
            duration: None,
            resolution: None,
        };
        assert_eq!(r.protocol(), "http-get");
        assert_eq!(r.mime(), "video/mp4");
        assert_eq!(r.byte_range(), Some(true));
        assert!(r.is_transcode());
    }

    /// `DLNA.ORG_OP=10` is time-seek but *not* byte-seek. Reading the wrong
    /// digit inverts the one flag the acceptance condition depends on.
    #[test]
    fn reads_the_byte_seek_digit_not_the_time_seek_digit() {
        let mut r = Res {
            url: "http://h/x".into(),
            protocol_info: "http-get:*:video/mp4:DLNA.ORG_OP=10".into(),
            size: None,
            duration: None,
            resolution: None,
        };
        assert_eq!(r.byte_range(), Some(false));
        r.protocol_info = "http-get:*:video/mp4:DLNA.ORG_OP=01".into();
        assert_eq!(r.byte_range(), Some(true));
        r.protocol_info = "http-get:*:video/mp4:".into();
        assert_eq!(r.byte_range(), None, "unstated is not the same as no");
    }

    /// An unseekable resource is still served — it plays, it just cannot be
    /// scrubbed — but `seekable` says so, so the failure is not blamed on the
    /// proxy.
    #[test]
    fn an_unseekable_choice_is_flagged_rather_than_refused() {
        let item = Item {
            id: "1".into(),
            title: "Only offer".into(),
            class: "object.item.videoItem".into(),
            resources: vec![Res {
                url: "http://h/x.mp4".into(),
                protocol_info: "http-get:*:video/mp4:DLNA.ORG_OP=00".into(),
                size: None,
                duration: None,
                resolution: None,
            }],
        };
        let chosen = choose_res(&item).unwrap();
        assert!(!chosen.seekable);
        assert!(chosen.reason.contains("scrubbing will not work"), "{}", chosen.reason);
    }

    #[test]
    fn prefers_the_original_over_a_transcode() {
        let item = Item {
            id: "1".into(),
            title: "Two offers".into(),
            class: "object.item.videoItem".into(),
            resources: vec![
                Res {
                    url: "http://h/transcode.mp4".into(),
                    protocol_info: "http-get:*:video/mp4:DLNA.ORG_OP=01;DLNA.ORG_CI=1".into(),
                    size: Some(999_999_999_999),
                    duration: None,
                    resolution: None,
                },
                Res {
                    url: "http://h/original.mp4".into(),
                    protocol_info: "http-get:*:video/mp4:DLNA.ORG_OP=01;DLNA.ORG_CI=0".into(),
                    size: Some(1_000),
                    duration: None,
                    resolution: None,
                },
            ],
        };
        // Size is the last tie-break precisely so it cannot outvote this.
        assert!(choose_res(&item).unwrap().res.url.ends_with("original.mp4"));
    }

    #[test]
    fn a_non_http_transport_is_never_chosen() {
        let item = Item {
            id: "1".into(),
            title: "RTSP only".into(),
            class: "object.item.videoItem".into(),
            resources: vec![Res {
                url: "rtsp://h/x".into(),
                protocol_info: "rtsp-rtp-udp:*:video/mp4:".into(),
                size: None,
                duration: None,
                resolution: None,
            }],
        };
        let err = choose_res(&item).unwrap_err();
        assert!(err.contains("no playable resource"), "{err}");
    }

    /// Malformed input must not panic and must not silently yield nothing
    /// where something parsed. What survived the break is returned.
    #[test]
    fn a_truncated_didl_yields_what_parsed() {
        let xml = br#"<DIDL-Lite><item id="1"><dc:title>One</dc:title></item>
                      <item id="2"><dc:title>Two</dc:tit"#;
        let listing = parse_didl(xml).unwrap();
        assert_eq!(listing.items.len(), 1);
        assert_eq!(listing.items[0].title, "One");
    }

    #[test]
    fn nonsense_input_is_an_empty_listing_not_a_panic() {
        for junk in [
            &b""[..],
            &b"not xml at all"[..],
            &b"<<<<>>>>"[..],
            &[0xff, 0xfe, 0x00, 0x01][..],
        ] {
            let listing = parse_didl(junk).unwrap_or_default();
            assert!(listing.items.is_empty());
        }
    }

    /// Deep nesting is refused rather than recursed into. The parser is
    /// iterative so this is a bound on work, not a stack-overflow guard, but
    /// the bound has to exist either way.
    #[test]
    fn absurd_nesting_is_refused() {
        let mut xml = Vec::new();
        for _ in 0..(MAX_DEPTH + 10) {
            xml.extend_from_slice(b"<a>");
        }
        assert!(parse_didl(&xml).is_err());
    }

    /// An `ObjectID` a client hands back must not be able to rewrite the SOAP
    /// body it is interpolated into.
    #[test]
    fn an_object_id_is_escaped_into_the_envelope() {
        assert_eq!(
            xml_escape(r#"1$7"</ObjectID><Evil>"#),
            "1$7&quot;&lt;/ObjectID&gt;&lt;Evil&gt;"
        );
    }
}
