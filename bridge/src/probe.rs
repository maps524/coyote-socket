//! Reachability check, run before the protocol attempt.
//!
//! The entire value of pointing this at a Quest is learning whether our
//! reading of the framing is right. That answer is only available if the
//! connection succeeds, so every *other* reason for failure has to be ruled
//! out first and named clearly — otherwise "wrong IP" and "our framing is
//! wrong" arrive looking the same, and the test tells us nothing.
//!
//! There is no ICMP here on purpose: raw sockets need privileges on every
//! platform and are blocked outright on plenty of networks. TCP connect
//! outcomes carry the same information for this question:
//!
//! | Outcome on :23554        | What it means                                     |
//! |--------------------------|---------------------------------------------------|
//! | accepted                 | Something is listening. Proceed to the protocol.  |
//! | refused (RST)            | Host is up and said "no". Remote control is off.  |
//! | timed out                | Nothing answered. Ambiguous on its own — hence the sibling probe below. |
//!
//! When 23554 times out we probe a couple of other ports on the same host. A
//! *refusal* from any of them proves the host is alive and answering, which
//! turns "wrong IP" into "the port is filtered". Those ports are expected to be
//! closed on a headset; the useful signal is which flavour of closed.

use std::time::Duration;

use serde::Serialize;
use tokio::net::TcpStream;

use crate::state::{FaultKind, LinkFault};

/// Long enough for a sleepy Wi-Fi headset, short enough that a wrong IP does
/// not feel like a hang.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Ports probed only to find out whether the *host* answers. Nothing is
/// expected to be listening on them; a refusal is the useful answer.
const LIVENESS_PORTS: [u16; 3] = [80, 443, 8080];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "kebab-case")]
pub enum Reachability {
    /// The target port accepted a connection. The protocol attempt can proceed.
    PortOpen,
    /// The host answered and refused the port. It is up; the player is not
    /// listening.
    PortClosed,
    /// The target port did not answer, but another port on the same host
    /// refused — so the host exists and something is filtering.
    HostUpPortFiltered,
    /// Nothing on that address answered anything.
    NoResponse,
    /// Not an address we could even try.
    BadAddress { detail: String },
}

impl Reachability {
    /// A one-line summary for a status area.
    pub fn summary(&self) -> String {
        match self {
            Self::PortOpen => "Something is listening. Connecting.".into(),
            Self::PortClosed => "Host answered, port closed — the player is not listening.".into(),
            Self::HostUpPortFiltered => {
                "Host is up but the port did not answer — filtered or blocked.".into()
            }
            Self::NoResponse => "No answer from that address.".into(),
            Self::BadAddress { detail } => format!("Not a usable address: {detail}"),
        }
    }

    /// What to do about it, or `None` when the probe found no problem.
    pub fn advice(&self) -> Option<String> {
        Some(match self {
            Self::PortOpen => return None,
            Self::PortClosed => "Open the player's settings and switch remote control on, then \
                 start a video and stay inside the video player. Neither DeoVR \
                 nor HereSphere opens the port before that."
                .into(),
            Self::HostUpPortFiltered => "The headset is there but the port is not reachable. Check the \
                 player is running with remote control enabled, and that no \
                 firewall sits between the two devices."
                .into(),
            Self::NoResponse => "Check the address, that the headset is awake, and that both \
                 devices are on the same network. The headset's IP is in its \
                 Wi-Fi settings and changes on DHCP."
                .into(),
            Self::BadAddress { .. } => {
                "Enter the headset's IP address, optionally followed by :23554.".into()
            }
        })
    }

    /// The corresponding link fault, for anything but a clean result.
    pub fn as_fault(&self) -> Option<LinkFault> {
        let kind = match self {
            Self::PortOpen => return None,
            Self::PortClosed => FaultKind::Refused,
            Self::HostUpPortFiltered | Self::NoResponse => FaultKind::TimedOut,
            Self::BadAddress { .. } => FaultKind::Address,
        };
        let mut fault = LinkFault::new(kind, self.summary());
        if let Some(advice) = self.advice() {
            fault = fault.with_hint(advice);
        }
        Some(fault)
    }
}

/// Probe `endpoint` (`host:port`) and say what kind of silence or answer it
/// gave.
pub async fn probe(endpoint: &str) -> Reachability {
    let Some((host, port)) = split_endpoint(endpoint) else {
        return Reachability::BadAddress {
            detail: format!("could not read a host and port from {endpoint:?}"),
        };
    };

    match connect_outcome(endpoint).await {
        Outcome::Accepted => Reachability::PortOpen,
        Outcome::Refused => Reachability::PortClosed,
        Outcome::Unresolvable(detail) => Reachability::BadAddress { detail },
        Outcome::Silent => {
            // Ambiguous. Ask the host something else and see whether it is
            // there at all.
            for probe_port in LIVENESS_PORTS {
                if probe_port == port {
                    continue;
                }
                if let Outcome::Refused | Outcome::Accepted =
                    connect_outcome(&format!("{host}:{probe_port}")).await
                {
                    return Reachability::HostUpPortFiltered;
                }
            }
            Reachability::NoResponse
        }
    }
}

enum Outcome {
    Accepted,
    Refused,
    Silent,
    Unresolvable(String),
}

async fn connect_outcome(endpoint: &str) -> Outcome {
    match tokio::time::timeout(PROBE_TIMEOUT, TcpStream::connect(endpoint)).await {
        Ok(Ok(_stream)) => Outcome::Accepted,
        Ok(Err(e)) => match e.kind() {
            std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::ConnectionReset => {
                Outcome::Refused
            }
            std::io::ErrorKind::TimedOut => Outcome::Silent,
            _ if e.to_string().to_lowercase().contains("unreachable") => Outcome::Silent,
            // A name that will not resolve, or a malformed address.
            _ => Outcome::Unresolvable(e.to_string()),
        },
        Err(_) => Outcome::Silent,
    }
}

/// Split `host:port`, tolerating a bracketed IPv6 literal.
///
/// Returns `None` rather than guessing: a bare host with no port has already
/// been filled in by the caller, and anything else is a typo worth reporting.
pub fn split_endpoint(endpoint: &str) -> Option<(String, u16)> {
    if let Some(rest) = endpoint.strip_prefix('[') {
        let (host, tail) = rest.split_once(']')?;
        let port = tail.strip_prefix(':')?.parse().ok()?;
        return Some((format!("[{host}]"), port));
    }
    let (host, port) = endpoint.rsplit_once(':')?;
    if host.is_empty() {
        return None;
    }
    Some((host.to_string(), port.parse().ok()?))
}

/// Fill in the well-known port when the user typed only an address. Typing the
/// IP is already the fiddly part.
pub fn normalise_endpoint(raw: &str, default_port: u16) -> String {
    let raw = raw.trim();
    if raw.starts_with('[') || raw.rsplit_once(':').is_some_and(|(_, p)| p.parse::<u16>().is_ok()) {
        raw.to_string()
    } else {
        format!("{raw}:{default_port}")
    }
}

/// Parse for the sake of validation only.
pub fn looks_like_an_endpoint(endpoint: &str) -> bool {
    split_endpoint(endpoint).is_some_and(|(host, port)| port > 0 && !host.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn an_open_port_probes_as_open() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = listener.accept().await;
        });
        assert_eq!(probe(&addr.to_string()).await, Reachability::PortOpen);
    }

    /// The case that matters most: the headset is there, the player is not
    /// listening. Binding then dropping a listener gives us a port on a host
    /// that is definitely up and definitely refusing.
    #[tokio::test]
    async fn a_closed_port_on_a_live_host_probes_as_closed() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let result = probe(&addr.to_string()).await;
        assert_eq!(result, Reachability::PortClosed);
        assert!(
            result.advice().unwrap().contains("remote control"),
            "the advice must name the actual cause"
        );
        assert_eq!(result.as_fault().unwrap().kind, FaultKind::Refused);
    }

    #[tokio::test]
    async fn an_unusable_address_is_reported_rather_than_probed() {
        let result = probe("not an address").await;
        assert!(matches!(result, Reachability::BadAddress { .. }));
        assert_eq!(result.as_fault().unwrap().kind, FaultKind::Address);
    }

    #[test]
    fn a_clean_probe_carries_no_fault() {
        assert!(Reachability::PortOpen.as_fault().is_none());
        assert!(Reachability::PortOpen.advice().is_none());
    }

    #[test]
    fn endpoints_split_including_ipv6() {
        assert_eq!(
            split_endpoint("192.168.1.50:23554"),
            Some(("192.168.1.50".into(), 23554))
        );
        assert_eq!(
            split_endpoint("[fe80::1]:23554"),
            Some(("[fe80::1]".into(), 23554))
        );
        assert_eq!(split_endpoint("192.168.1.50"), None);
        assert_eq!(split_endpoint(":23554"), None);
    }

    #[test]
    fn a_bare_host_gains_the_default_port() {
        assert_eq!(normalise_endpoint("192.168.1.50", 23554), "192.168.1.50:23554");
        assert_eq!(
            normalise_endpoint(" 192.168.1.50:9000 ", 23554),
            "192.168.1.50:9000"
        );
        assert_eq!(normalise_endpoint("[fe80::1]:1", 23554), "[fe80::1]:1");
    }

    #[test]
    fn validation_rejects_the_obvious_typos() {
        assert!(looks_like_an_endpoint("10.0.0.2:23554"));
        assert!(!looks_like_an_endpoint("10.0.0.2"));
        assert!(!looks_like_an_endpoint("10.0.0.2:notaport"));
        assert!(!looks_like_an_endpoint(""));
    }
}
