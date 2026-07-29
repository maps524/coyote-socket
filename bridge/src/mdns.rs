//! Answers `coyote.local` on the LAN, so the phone has an address that does not move.
//!
//! ## Why the bridge has to do this itself
//!
//! The obvious idea is to lean on the machine's own name — Windows and macOS
//! both answer `<hostname>.local` without any help. That was measured on the
//! development machine and it is **actively wrong**:
//!
//! ```text
//! ping JUSTIN-G.local  ->  172.21.160.1     (vEthernet, WSL's virtual switch)
//! actual LAN address   ->  192.168.0.9      (Ethernet)
//! ```
//!
//! Windows' built-in responder advertised a virtual adapter, which is not
//! reachable from a phone at all. That is the same class of bug the comment on
//! [`crate::http::local_ip`] already warns about — "the first interface is
//! usually a virtual adapter" — and it means the machine hostname cannot be
//! trusted to produce a routable answer. So the bridge runs its own responder
//! and advertises exactly the address it chose, which is also exactly the
//! address in the certificate's IP SAN. One decision, used in both places.
//!
//! The second reason is that a fixed name is the whole point. `coyote.local`
//! stays put when DHCP moves the machine; an IP does not. A changed address is
//! a changed origin, and a changed origin wipes OPFS, kills the PWA install and
//! resets the Web Bluetooth device grant — the exact churn that ruled out
//! tunnels. iOS resolves `.local` natively, with no app and no configuration.
//!
//! ## What this does not solve
//!
//! mDNS is a LAN protocol and some networks suppress multicast — guest SSIDs
//! and "client isolation" on consumer access points are the usual culprits.
//! When that happens the name will not resolve and nothing here can fix it,
//! which is why the certificate also carries the current IP as a SAN and the
//! install page can fall back to it. The fallback is a worse experience, not a
//! broken one: it works until the address changes.

use std::collections::HashMap;
use std::net::IpAddr;

use mdns_sd::{ServiceDaemon, ServiceInfo};

use crate::certs::BRIDGE_HOSTNAME;
use crate::{log_info, log_warn};

/// Service type. `_http._tcp` is the honest label — a browser is what connects
/// — and it makes the bridge visible to any generic discovery browser, which is
/// a useful diagnostic when someone is working out whether multicast reaches
/// their phone at all.
const SERVICE_TYPE: &str = "_http._tcp.local.";

/// The registration handle. Dropping it withdraws the name from the network,
/// so callers must keep it alive for as long as the bridge is serving.
pub struct Responder {
    _daemon: ServiceDaemon,
}

/// Start answering `coyote.local` with `ip`.
///
/// `https_port` is what the service record advertises, because the HTTPS
/// listener is the one the phone is meant to end up on.
///
/// Failure is deliberately not fatal. A machine with multicast blocked, or an
/// mDNS port it cannot share, still serves perfectly well over its IP address —
/// the user simply loses the stable name. Refusing to start the bridge over it
/// would trade a degraded feature for no feature at all.
pub fn start(ip: IpAddr, https_port: u16) -> Option<Responder> {
    let daemon = match ServiceDaemon::new() {
        Ok(d) => d,
        Err(e) => {
            log_warn!(
                "[mdns] could not start the responder ({e}); \
                 {BRIDGE_HOSTNAME} will not resolve. The bridge is still reachable at its IP address."
            );
            return None;
        }
    };

    // mdns-sd requires the trailing dot: a hostname here is a DNS name, not a
    // display string, and it rejects one without it.
    let host_fqdn = format!("{BRIDGE_HOSTNAME}.");
    let info = match ServiceInfo::new(
        SERVICE_TYPE,
        "CoyoteSocket Bridge",
        &host_fqdn,
        ip,
        https_port,
        HashMap::<String, String>::new(),
    ) {
        Ok(info) => info,
        Err(e) => {
            log_warn!("[mdns] could not describe the service ({e}); {BRIDGE_HOSTNAME} will not resolve");
            return None;
        }
    };

    if let Err(e) = daemon.register(info) {
        log_warn!("[mdns] could not register {BRIDGE_HOSTNAME} ({e}); the bridge is still reachable at {ip}");
        return None;
    }

    log_info!("[mdns] answering {BRIDGE_HOSTNAME} with {ip} on port {https_port}");
    Some(Responder { _daemon: daemon })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn the_advertised_hostname_is_the_one_the_certificate_is_issued_for() {
        // These two must agree or the phone gets a name it can resolve and a
        // certificate that does not cover it — which presents as a TLS error
        // with no hint that mDNS is involved.
        assert_eq!(BRIDGE_HOSTNAME, "coyote.local");
        assert!(format!("{BRIDGE_HOSTNAME}.").ends_with(".local."));
    }

    #[test]
    fn a_responder_starts_and_stops_without_taking_the_bridge_with_it() {
        // Multicast may well be unavailable wherever this test runs, so the
        // assertion is about the failure being survivable, not about success.
        let responder = start(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8443);
        drop(responder);
    }
}
