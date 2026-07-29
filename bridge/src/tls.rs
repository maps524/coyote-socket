//! The HTTPS listener, and the task that keeps its certificate current.
//!
//! This is a thin module on purpose. All the routing lives in [`crate::http`]
//! and works over any stream, so serving TLS is: accept, handshake, hand the
//! decrypted stream to the same `serve_conn` the plain listener uses. The one
//! difference that matters is the `secure` flag, which is passed `true` here
//! and `false` there — token rotation refuses over plaintext, and this is the
//! listener that makes rotation possible at all.
//!
//! ## Why both listeners stay up
//!
//! Plain HTTP is not a fallback that should be removed once TLS works. It is
//! load-bearing:
//!
//! - The install page **must** live on it. A phone that has not yet trusted the
//!   local CA meets a full-page certificate interstitial on HTTPS with no route
//!   back to the instructions, so the QR has to point somewhere plaintext.
//! - Everything except Web Bluetooth works fine over it, and it is far easier
//!   to debug. HTTPS is required for the Coyote connection specifically, not
//!   for the app in general.
//!
//! ## What this does not give you
//!
//! **A completed TLS handshake is not authorization.** Every device on the LAN
//! that has installed the CA — which is every device the user pointed at the
//! install page, and any that got there by itself — can handshake exactly as
//! successfully as the phone. Authorization is the pairing token in
//! [`crate::auth`], and it is a different mechanism answering a different
//! attacker. Neither is a substitute for the other, and neither on its own
//! makes the sentence "the bridge is secure" true.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_rustls::TlsAcceptor;

use crate::certs::{self, CertMaterial, LocalCa};
use crate::http;
use crate::{log_debug, log_info, log_warn};

/// How often to check whether the leaf still matches reality.
///
/// Six hours is a compromise between noticing a DHCP move reasonably soon and
/// not spending a machine's life re-issuing certificates. A move is not
/// instantaneous to recover from either way — the phone has a cached DNS answer
/// — so a tighter loop would buy very little.
const REISSUE_CHECK: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

/// Serve HTTPS until the process ends.
///
/// `certs_rx` carries the current TLS configuration. It is a watch channel
/// rather than a fixed value so [`keep_current`] can swap the certificate
/// underneath a running listener: connections already established keep the
/// configuration they handshook with, and new ones pick up the replacement.
pub async fn run(listener: TcpListener, ctx: Arc<http::Ctx>, certs_rx: watch::Receiver<CertMaterial>) {
    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                // Read the configuration per-connection so a re-issued leaf
                // takes effect without restarting the listener.
                let acceptor = TlsAcceptor::from(certs_rx.borrow().server_config.clone());
                let ctx = Arc::clone(&ctx);
                tokio::spawn(async move {
                    match acceptor.accept(stream).await {
                        Ok(stream) => {
                            // `secure: true` — this is the transport that
                            // `/pair/rotate` requires.
                            http::serve_conn(stream, addr, ctx, true).await
                        }
                        // A failed handshake is the *expected* outcome for a
                        // phone that has not installed the CA yet, so this is
                        // debug rather than a warning. It would otherwise fill
                        // the log during the exact flow the install page exists
                        // to walk someone through.
                        Err(e) => log_debug!("[tls] {addr} handshake failed: {e}"),
                    }
                });
            }
            Err(e) => {
                log_warn!("[tls] accept failed: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
        }
    }
}

/// Re-issue the leaf when the machine's address changes or expiry approaches.
///
/// Both are cases that would otherwise present to the user as "it stopped
/// working for no reason", and neither requires the phone to do anything: the
/// CA is unchanged, so a device that trusted the old leaf trusts the new one.
/// That is the whole reason the CA is persisted and the leaf is not.
pub async fn keep_current(ca: Arc<LocalCa>, certs_tx: watch::Sender<CertMaterial>) {
    let mut issued_at = time::OffsetDateTime::now_utc();

    loop {
        tokio::time::sleep(REISSUE_CHECK).await;

        let ips: Vec<IpAddr> = http::local_ip().into_iter().collect();
        let current = certs_tx.borrow().leaf_names.clone();
        if !certs::needs_reissue(&current, &ips, issued_at) {
            continue;
        }

        match ca.issue_leaf(&ips) {
            Ok(material) => {
                log_info!(
                    "[tls] re-issued the certificate for {} — no action is needed on any phone, \
                     because the authority they trust has not changed",
                    material.leaf_names.join(", ")
                );
                issued_at = time::OffsetDateTime::now_utc();
                let _ = certs_tx.send(material);
            }
            // Keep serving the old certificate. It is still valid — this ran
            // because it was *approaching* expiry, not past it — and dropping
            // TLS entirely would be a far worse outcome than a stale IP SAN.
            Err(e) => log_warn!("[tls] could not re-issue the certificate ({e}); continuing with the current one"),
        }
    }
}

/// Everything a caller needs to start serving TLS.
pub struct Prepared {
    pub ca: Arc<LocalCa>,
    pub material: CertMaterial,
    /// Public half, for the install page. No key material.
    pub public: Arc<crate::install::TlsPublicInfo>,
}

/// Load or create the CA, issue a leaf, and describe the result.
///
/// Returns `Err` only for things that make HTTPS impossible. The caller is
/// expected to carry on serving plain HTTP in that case rather than exiting:
/// losing the secure context costs Web Bluetooth, and it should cost nothing
/// else.
pub fn prepare(
    config_dir: &std::path::Path,
    http_port: u16,
    https_port: u16,
) -> Result<Prepared, String> {
    let dir = certs::tls_dir(config_dir);
    let ca = LocalCa::load_or_generate(&dir)?;
    let ip = http::local_ip();
    let ips: Vec<IpAddr> = ip.into_iter().collect();
    let material = ca.issue_leaf(&ips)?;

    let public = Arc::new(crate::install::TlsPublicInfo {
        ca_cert_pem: material.ca_cert_pem.clone(),
        ca_display_name: material.ca_display_name.clone(),
        hostname: certs::BRIDGE_HOSTNAME.to_string(),
        http_port,
        https_port,
        ip,
    });

    Ok(Prepared {
        ca: Arc::new(ca),
        material,
        public,
    })
}

/// Origins a browser may legitimately present once TLS is in play.
///
/// The `Origin` allowlist on the WebSocket upgrade is keyed on `host:port`, and
/// TLS introduces new ones: the mDNS name, the HTTPS port, and the IP fallback.
/// Missing any of them means the phone loads the app over HTTPS and then has
/// its socket refused — which looks like a bridge fault and is a configuration
/// one, so they are generated in one place rather than typed into two.
pub fn browser_origins(ip: Option<IpAddr>, http_port: u16, https_port: u16) -> Vec<String> {
    let mut hosts = Vec::new();
    for host in [certs::BRIDGE_HOSTNAME.to_string(), "localhost".to_string()]
        .into_iter()
        .chain(ip.map(|ip| ip.to_string()))
        .chain(std::iter::once("127.0.0.1".to_string()))
    {
        hosts.push(format!("{host}:{https_port}"));
        hosts.push(format!("{host}:{http_port}"));
    }
    hosts.sort();
    hosts.dedup();
    hosts
}

/// Bind the HTTPS listener.
pub async fn bind(bind_ip: IpAddr, port: u16) -> Result<TcpListener, String> {
    let addr = SocketAddr::new(bind_ip, port);
    TcpListener::bind(addr)
        .await
        .map_err(|e| format!("could not bind {addr}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn every_origin_the_phone_can_present_is_allowed() {
        let ip = Some(IpAddr::V4(Ipv4Addr::new(192, 168, 0, 9)));
        let origins = browser_origins(ip, 8787, 8443);

        // The one that matters: the app is served from the mDNS name over
        // HTTPS, so that is the Origin its WebSocket upgrade will carry. Omit
        // it and the phone loads the app and is then refused its own socket.
        assert!(origins.contains(&"coyote.local:8443".to_string()));
        // The install page's own origin, which is where the trust check and
        // the rotation fetch are issued from.
        assert!(origins.contains(&"coyote.local:8787".to_string()));
        // IP fallback, for networks that eat multicast.
        assert!(origins.contains(&"192.168.0.9:8443".to_string()));
        // Desktop testing.
        assert!(origins.contains(&"127.0.0.1:8787".to_string()));
    }

    #[test]
    fn a_machine_with_no_detectable_address_still_produces_usable_origins() {
        let origins = browser_origins(None, 8787, 8443);
        assert!(origins.contains(&"coyote.local:8443".to_string()));
        assert!(origins.contains(&"localhost:8787".to_string()));
    }

    #[test]
    fn origins_do_not_repeat() {
        // `local_ip` can legitimately return loopback, which would otherwise
        // produce a duplicate entry.
        let ip = Some(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)));
        let mut sorted = browser_origins(ip, 8787, 8443);
        let before = sorted.len();
        sorted.dedup();
        assert_eq!(before, sorted.len());
    }
}
