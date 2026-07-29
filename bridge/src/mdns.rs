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

/// Instance name. Constant because re-registering under the same name *updates*
/// the record; a name that varied would leave stale entries behind on every
/// address change.
const INSTANCE_NAME: &str = "CoyoteSocket Bridge";

/// The registration handle. Dropping it withdraws the name from the network,
/// so callers must keep it alive for as long as the bridge is serving.
pub struct Responder {
    daemon: ServiceDaemon,
    https_port: u16,
}

impl Responder {
    /// Announce `ip` for [`BRIDGE_HOSTNAME`], replacing any previous answer.
    ///
    /// Re-registering under the same instance name updates the record rather
    /// than adding a second one, and the announcement is multicast immediately,
    /// so listeners converge without waiting for their caches to expire.
    pub fn announce(&self, ip: IpAddr) -> bool {
        let host_fqdn = format!("{BRIDGE_HOSTNAME}.");
        let info = match ServiceInfo::new(
            SERVICE_TYPE,
            INSTANCE_NAME,
            &host_fqdn,
            ip,
            self.https_port,
            HashMap::<String, String>::new(),
        ) {
            Ok(info) => info,
            Err(e) => {
                log_warn!("[mdns] could not describe the service ({e}); {BRIDGE_HOSTNAME} will not resolve");
                return false;
            }
        };

        if let Err(e) = self.daemon.register(info) {
            log_warn!("[mdns] could not register {BRIDGE_HOSTNAME} ({e}); the bridge is still reachable at {ip}");
            return false;
        }
        true
    }
}

/// Keep the announced address in step with the machine's actual one.
///
/// Without this the stable name fails in exactly the situation it exists for:
/// DHCP moves the machine, a fresh certificate is issued for the new address —
/// so the phone never reinstalls, which is the part that holds — and
/// `coyote.local` goes on announcing an address nothing is listening on. The
/// name would be stable and wrong, which is worse than unstable and right,
/// because nothing in the failure points at DNS.
/// Returns a future the **caller** spawns, rather than spawning itself.
///
/// It used to call `tokio::spawn` directly, which panicked with "there is no
/// reactor running" for the Tauri app: `setup()` runs on a plain `main` with no
/// runtime entered, so the app crashed on launch on its default path. The
/// headless binary happened to be fine only because it called this from inside
/// an already-spawned task.
///
/// A library that spawns has to be right about which runtime it is on; a
/// library that returns a future cannot be wrong. The two callers here are on
/// different runtimes — Tauri's and plain tokio's — which is exactly the
/// situation where that distinction stops being stylistic.
pub async fn follow(responder: Responder, mut rx: tokio::sync::watch::Receiver<IpAddr>) {
    // Held for the lifetime of the future: dropping the responder withdraws the
    // name from the network.
    let responder = responder;
    while rx.changed().await.is_ok() {
        let ip = *rx.borrow_and_update();
        if responder.announce(ip) {
            log_info!("[mdns] {BRIDGE_HOSTNAME} now answers with {ip}");
        }
    }
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

    let responder = Responder { daemon, https_port };
    if !responder.announce(ip) {
        return None;
    }

    log_info!("[mdns] answering {BRIDGE_HOSTNAME} with {ip} on port {https_port}");
    Some(responder)
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
    #[ignore = "announces coyote.local on the real LAN; see the comment"]
    fn a_responder_starts_and_stops_without_taking_the_bridge_with_it() {
        // IGNORED BY DEFAULT, and it must stay that way.
        //
        // `start` does not simulate anything — it multicasts a real
        // announcement on every interface. Run in a normal `cargo test` it
        // publishes `coyote.local -> 127.0.0.1` to the whole network, and any
        // phone that caches that answer then tries to reach the bridge at its
        // own loopback. The bridge is fine, the certificate is fine, and the
        // phone cannot connect — with the cause sitting in a unit test that
        // finished milliseconds later.
        //
        // That is a worse failure than the one this test checks for. Run it
        // deliberately with `--ignored`, on a network where poisoning a cache
        // does not matter.
        let responder = start(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8443);
        drop(responder);
    }
}
