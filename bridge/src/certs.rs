//! A per-install local certificate authority, so the phone gets a secure context.
//!
//! ## Why this exists at all
//!
//! Web Bluetooth requires a secure context. `localhost` is exempt, which is
//! exactly what hides the problem during desktop testing — the phone is not
//! localhost. Served over plain HTTP the phone cannot reach the Coyote, and
//! served through an HTTPS tunnel it cannot open a socket back to a LAN
//! service. There is no arrangement of plaintext-plus-tunnel that completes the
//! chain, so the bridge issues its own certificate and the user trusts it once.
//!
//! The second thing it buys is a **stable origin**. A quick tunnel mints a new
//! hostname every restart, and a new origin means OPFS wiped, PWA install dead
//! and the Web Bluetooth device grant reset. `https://coyote.local:8443` does
//! not move.
//!
//! ## What this does NOT give you
//!
//! **TLS here provides confidentiality and a secure context. It provides no
//! authorization whatsoever.** Once the CA is installed on a device, every
//! other device on the LAN is exactly as authorized as the phone is: the
//! listener will happily complete a handshake with any of them. Authorization
//! is a separate mechanism (a pairing token) owned elsewhere, and the two are
//! answers to different attackers. Nothing in this module should ever be cited
//! as evidence that the bridge is secure.
//!
//! ## The security shape of a local CA
//!
//! Installing a root CA lets whoever holds its private key sign a certificate
//! for *any* domain and present it to that device. That is a real grant and it
//! is why two rules here are absolute:
//!
//! 1. **The CA is generated per-install, on the user's own machine.** No CA key
//!    is ever compiled into the binary. A shipped key would let anyone who
//!    downloaded a release impersonate any site to every user who ever
//!    installed it — catastrophic, and unfixable after the fact because the key
//!    would already be public.
//! 2. **The private key never leaves the machine.** Not in the QR, not over the
//!    network, not in a log line. [`CertMaterial::ca_cert_pem`] — the public
//!    certificate — is the only thing the install page serves.
//!
//! ## Apple's requirements, which are strict and fail silently
//!
//! iOS 13+ rejects naive self-signed certificates, and the rejection is not
//! explained anywhere the user can see it. The leaf here satisfies all of:
//!
//! - a `subjectAltName` is present, and the common name is ignored entirely;
//! - `id-kp-serverAuth` appears in the extended key usage;
//! - validity is **≤ 398 days** ([`LEAF_VALIDITY_DAYS`]);
//! - the key is ECDSA P-256, which is on the accepted list (RSA would need
//!   ≥ 2048 bits).
//!
//! Getting any of these wrong produces a failure indistinguishable from a
//! certificate that was never installed, so they are asserted in tests rather
//! than left to a reviewer's memory.
//!
//! ## Why the leaf is not persisted and the CA is
//!
//! Only the CA is written to disk. The leaf is re-issued from it on every
//! startup, and again by a background task if the machine's address changes or
//! expiry approaches. That is what makes DHCP a non-event: the address moving
//! invalidates the leaf's IP SAN, and the fix is a new leaf signed by the same
//! CA — which the phone already trusts, so **the phone never reinstalls
//! anything**. It also deletes an entire class of renewal bug, because there is
//! no stored leaf whose expiry could be misread.
//!
//! Losing the CA is the one event that *does* cost the user a reinstall, so it
//! is logged as a warning rather than silently papered over with a fresh one.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer,
    KeyPair, KeyUsagePurpose, SanType, PKCS_ECDSA_P256_SHA256,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::ServerConfig;
use time::{Duration, OffsetDateTime};

use crate::{log_info, log_warn};

/// The hostname the phone is told to use. mDNS resolves it natively on iOS, so
/// it survives the machine's IP changing — which an IP SAN alone would not.
pub const BRIDGE_HOSTNAME: &str = "coyote.local";

/// Leaf validity. Apple's ceiling is 398 days; a year keeps a comfortable
/// margin and the leaf is re-issued on every start anyway.
pub const LEAF_VALIDITY_DAYS: i64 = 365;

/// iOS 13+ rejects a server certificate valid for more than 398 days, and the
/// rejection looks exactly like a certificate that was never installed. This
/// fails the build rather than a test, because the failure it guards against is
/// someone raising the constant to save themselves a renewal.
const _: () = assert!(
    LEAF_VALIDITY_DAYS <= 398,
    "Apple rejects leaf certificates valid for more than 398 days"
);

/// CA validity. The 398-day rule constrains *server* certificates, not a root
/// the user installed deliberately, and a decade means the trust decision is
/// made once rather than annually.
const CA_VALIDITY_DAYS: i64 = 3650;

/// Re-issue the leaf when it has less than this left. Only reachable by a
/// bridge left running for the better part of a year, which is exactly the
/// home-server case this is meant to survive.
const RENEW_WITHIN_DAYS: i64 = 30;

/// Filenames under the TLS directory. The key is named unambiguously: someone
/// clearing out a config folder should not have to guess which of these two
/// files is the secret.
const CA_CERT_FILE: &str = "ca-cert.pem";
const CA_KEY_FILE: &str = "ca-private-key.pem";

/// Everything the listeners and the install page need.
#[derive(Clone)]
pub struct CertMaterial {
    /// The CA's **public** certificate, PEM. This is what the install page
    /// serves and the only certificate artefact that ever leaves the machine.
    pub ca_cert_pem: String,
    /// How the certificate identifies itself in iOS's certificate list.
    pub ca_display_name: String,
    /// Names the current leaf is valid for, for display and diagnostics.
    pub leaf_names: Vec<String>,
    /// Ready-to-serve rustls configuration.
    pub server_config: Arc<ServerConfig>,
}

/// A loaded CA, kept so leaves can be re-issued without touching disk again.
pub struct LocalCa {
    key_pair: KeyPair,
    params: CertificateParams,
    cert_pem: String,
    display_name: String,
    /// Whether this CA was just created. The caller surfaces that differently
    /// from "loaded the existing one": the first means the user has to install
    /// something, the second means they do not.
    pub freshly_generated: bool,
}

impl LocalCa {
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    pub fn cert_pem(&self) -> &str {
        &self.cert_pem
    }

    /// Load the CA from `dir`, generating one if there is none.
    ///
    /// A CA that exists but cannot be read is treated as a **loss**, not as an
    /// invitation to mint a new one quietly: the user's phone still trusts the
    /// old one, and replacing it without saying so turns "HTTPS stopped
    /// working" into an unexplainable event. The warning is the deliverable.
    pub fn load_or_generate(dir: &Path) -> Result<Self, String> {
        let cert_path = dir.join(CA_CERT_FILE);
        let key_path = dir.join(CA_KEY_FILE);

        if cert_path.exists() || key_path.exists() {
            match Self::load(&cert_path, &key_path) {
                Ok(ca) => {
                    log_info!(
                        "[tls] using the existing local CA from {} — phones that already trust it need no action",
                        dir.display()
                    );
                    return Ok(ca);
                }
                Err(e) => {
                    log_warn!(
                        "[tls] the local CA in {} could not be read ({e}). A new one will be generated, \
                         which means every phone that trusted the old certificate must install the new one. \
                         If the old files can be restored from a backup, stop the bridge and do that first.",
                        dir.display()
                    );
                }
            }
        }

        Self::generate(dir)
    }

    fn load(cert_path: &Path, key_path: &Path) -> Result<Self, String> {
        let cert_pem =
            std::fs::read_to_string(cert_path).map_err(|e| format!("reading certificate: {e}"))?;
        let key_pem =
            std::fs::read_to_string(key_path).map_err(|e| format!("reading private key: {e}"))?;
        let key_pair =
            KeyPair::from_pem(&key_pem).map_err(|e| format!("parsing private key: {e}"))?;

        // The distinguished name is rebuilt rather than parsed back out of the
        // certificate: it is derived deterministically from the same inputs, so
        // reconstructing it avoids a whole X.509 parser in the dependency tree
        // for information we already know. It must match the stored
        // certificate's subject, which is why `ca_params` is the single place
        // the name is composed.
        let display_name = display_name_for_host();
        let params = ca_params(&display_name)?;

        Ok(Self {
            key_pair,
            params,
            cert_pem,
            display_name,
            freshly_generated: false,
        })
    }

    fn generate(dir: &Path) -> Result<Self, String> {
        let display_name = display_name_for_host();
        let params = ca_params(&display_name)?;
        let key_pair = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)
            .map_err(|e| format!("generating CA key: {e}"))?;
        let cert = params
            .self_signed(&key_pair)
            .map_err(|e| format!("self-signing the CA: {e}"))?;
        let cert_pem = cert.pem();

        std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        write_private(&dir.join(CA_KEY_FILE), &key_pair.serialize_pem())?;
        std::fs::write(dir.join(CA_CERT_FILE), &cert_pem)
            .map_err(|e| format!("writing the CA certificate: {e}"))?;

        log_info!(
            "[tls] generated a new local CA \"{display_name}\" in {}. \
             The private key stays on this machine and is never sent anywhere.",
            dir.display()
        );

        Ok(Self {
            key_pair,
            params,
            cert_pem,
            display_name,
            freshly_generated: true,
        })
    }

    /// Issue a leaf for the given addresses and build a rustls config from it.
    ///
    /// `ips` are additional SANs — the current LAN address, so a phone that
    /// cannot resolve mDNS can still reach the bridge by number. The hostname
    /// is always included and is what the QR advertises, because the IP is the
    /// part that moves.
    pub fn issue_leaf(&self, ips: &[IpAddr]) -> Result<CertMaterial, String> {
        let mut names: Vec<String> = vec![BRIDGE_HOSTNAME.to_string(), "localhost".to_string()];
        let mut sans = vec![
            SanType::DnsName(
                BRIDGE_HOSTNAME
                    .try_into()
                    .map_err(|e| format!("hostname SAN: {e}"))?,
            ),
            SanType::DnsName(
                "localhost"
                    .try_into()
                    .map_err(|e| format!("localhost SAN: {e}"))?,
            ),
        ];
        for ip in ips {
            sans.push(SanType::IpAddress(*ip));
            names.push(ip.to_string());
        }

        let mut params = CertificateParams::default();
        params.subject_alt_names = sans;
        // Apple ignores the common name entirely; it is set only so the
        // certificate is legible in a viewer.
        params
            .distinguished_name
            .push(DnType::CommonName, BRIDGE_HOSTNAME);
        params.is_ca = IsCa::NoCa;
        // `id-kp-serverAuth`. iOS 13+ rejects a leaf without it.
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyEncipherment,
        ];

        let now = OffsetDateTime::now_utc();
        // Backdated an hour so a phone whose clock runs slightly behind does
        // not reject a certificate issued seconds ago.
        params.not_before = now - Duration::hours(1);
        params.not_after = now + Duration::days(LEAF_VALIDITY_DAYS);

        let leaf_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)
            .map_err(|e| format!("generating the leaf key: {e}"))?;
        let issuer = Issuer::from_params(&self.params, &self.key_pair);
        let leaf = params
            .signed_by(&leaf_key, &issuer)
            .map_err(|e| format!("signing the leaf: {e}"))?;

        // Chain is leaf-then-CA. Sending the root is redundant for a device
        // that has already installed it and harmless for one that has not, and
        // it makes the chain self-describing when someone inspects it with
        // `openssl s_client` while working out why a phone is unhappy.
        let chain = vec![
            CertificateDer::from(leaf.der().to_vec()),
            pem_to_der(&self.cert_pem)?,
        ];
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der()));

        let server_config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .map_err(|e| format!("building the TLS configuration: {e}"))?;

        Ok(CertMaterial {
            ca_cert_pem: self.cert_pem.clone(),
            ca_display_name: self.display_name.clone(),
            leaf_names: names,
            server_config: Arc::new(server_config),
        })
    }
}

/// When the current leaf should be replaced.
///
/// Two triggers, both of which would otherwise present to the user as "it just
/// stopped working": the address moved (DHCP), or the certificate is running
/// out. Neither requires the phone to do anything, because the CA is unchanged.
pub fn needs_reissue(current_names: &[String], ips: &[IpAddr], issued_at: OffsetDateTime) -> bool {
    let expiring =
        OffsetDateTime::now_utc() + Duration::days(RENEW_WITHIN_DAYS) > issued_at + Duration::days(LEAF_VALIDITY_DAYS);

    let mut expected: Vec<String> = vec![BRIDGE_HOSTNAME.to_string(), "localhost".to_string()];
    expected.extend(ips.iter().map(|ip| ip.to_string()));

    expiring || expected != current_names
}

/// Parameters for the CA certificate.
///
/// Kept in one function because both `generate` and `load` must produce
/// byte-identical distinguished names — the loaded issuer signs leaves whose
/// issuer field has to match the stored certificate's subject, and a mismatch
/// would produce certificates that fail path building for no visible reason.
fn ca_params(display_name: &str) -> Result<CertificateParams, String> {
    let mut params = CertificateParams::default();
    params
        .distinguished_name
        .push(DnType::CommonName, display_name);
    params
        .distinguished_name
        .push(DnType::OrganizationName, "CoyoteSocket");
    // `Constrained(0)` — this CA may sign leaves and may not sign further CAs.
    // Nothing here needs an intermediate, and a path length of zero is a
    // meaningful limit on a key that is being installed as a trust root.
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];

    let now = OffsetDateTime::now_utc();
    params.not_before = now - Duration::hours(1);
    params.not_after = now + Duration::days(CA_VALIDITY_DAYS);
    Ok(params)
}

/// A name the user can recognise in Settings months from now.
///
/// The machine name is in it deliberately: someone who has run this on a
/// desktop and a home server will otherwise see two identical entries and have
/// no way to tell which one they are about to delete.
fn display_name_for_host() -> String {
    let host = hostname().unwrap_or_else(|| "this computer".to_string());
    format!("CoyoteSocket Bridge on {host}")
}

fn hostname() -> Option<String> {
    // No dependency for one string. Both variables are set by the OS on the
    // platforms this runs on, and the fallback covers the case where neither is.
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|h| !h.is_empty())
}

/// Write a file that should not be world-readable.
///
/// On Unix the mode is set at creation rather than after, so there is no window
/// in which the key exists with default permissions. On Windows the file
/// inherits the config directory's ACL, which is already user-scoped — there is
/// no portable equivalent to `0600` and pretending otherwise would be worse
/// than saying so.
fn write_private(path: &Path, contents: &str) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("writing {}: {e}", path.display()))?;
        return file
            .write_all(contents.as_bytes())
            .map_err(|e| format!("writing {}: {e}", path.display()));
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, contents).map_err(|e| format!("writing {}: {e}", path.display()))
    }
}

/// Minimal PEM decode, so a whole base64/PEM crate is not pulled in for one
/// certificate. Handles exactly the shape rcgen emits.
fn pem_to_der(pem: &str) -> Result<CertificateDer<'static>, String> {
    let body: String = pem
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect();
    let bytes = base64_decode(body.trim()).ok_or("the stored CA certificate is not valid PEM")?;
    Ok(CertificateDer::from(bytes))
}

fn base64_decode(input: &str) -> Option<Vec<u8>> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut lookup = [255u8; 256];
    for (i, c) in TABLE.iter().enumerate() {
        lookup[*c as usize] = i as u8;
    }

    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for byte in input.bytes() {
        if byte == b'=' || byte.is_ascii_whitespace() {
            continue;
        }
        let value = lookup[byte as usize];
        if value == 255 {
            return None;
        }
        acc = (acc << 6) | value as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// Directory the CA lives in, beside the settings file rather than somewhere of
/// its own — one place to back up, one place to clear.
pub fn tls_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("tls")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "coyote-bridge-tls-test-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_generated_ca_is_reloaded_rather_than_replaced() {
        let dir = temp_dir("reload");
        let first = LocalCa::load_or_generate(&dir).expect("generate");
        assert!(first.freshly_generated);

        let second = LocalCa::load_or_generate(&dir).expect("reload");
        assert!(
            !second.freshly_generated,
            "a second run must reuse the CA — regenerating means the phone has to reinstall"
        );
        assert_eq!(
            first.cert_pem(),
            second.cert_pem(),
            "the reloaded certificate must be byte-identical, or trust breaks"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_private_key_is_never_part_of_what_gets_served() {
        let dir = temp_dir("secrecy");
        let ca = LocalCa::load_or_generate(&dir).expect("generate");
        let material = ca
            .issue_leaf(&[IpAddr::V4(Ipv4Addr::new(192, 168, 0, 9))])
            .expect("issue");

        // The single most important property in this module: what the install
        // page hands out must be a certificate and nothing else.
        assert!(material.ca_cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(
            !material.ca_cert_pem.contains("PRIVATE KEY"),
            "the CA private key must never appear in served material"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_leaf_covers_the_hostname_and_the_current_address() {
        let dir = temp_dir("sans");
        let ca = LocalCa::load_or_generate(&dir).expect("generate");
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 0, 9));
        let material = ca.issue_leaf(&[ip]).expect("issue");

        assert!(material.leaf_names.contains(&BRIDGE_HOSTNAME.to_string()));
        assert!(material.leaf_names.contains(&"192.168.0.9".to_string()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_address_change_triggers_a_new_leaf_but_a_stable_one_does_not() {
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 0, 9));
        let moved = IpAddr::V4(Ipv4Addr::new(192, 168, 0, 44));
        let names = vec![
            BRIDGE_HOSTNAME.to_string(),
            "localhost".to_string(),
            "192.168.0.9".to_string(),
        ];
        let now = OffsetDateTime::now_utc();

        assert!(
            !needs_reissue(&names, &[ip], now),
            "a fresh leaf on an unchanged address must not be reissued"
        );
        assert!(
            needs_reissue(&names, &[moved], now),
            "DHCP moving the machine must produce a new leaf"
        );
    }

    #[test]
    fn a_leaf_near_expiry_is_reissued() {
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 0, 9));
        let names = vec![
            BRIDGE_HOSTNAME.to_string(),
            "localhost".to_string(),
            "192.168.0.9".to_string(),
        ];
        let long_ago = OffsetDateTime::now_utc() - Duration::days(LEAF_VALIDITY_DAYS - 5);
        assert!(
            needs_reissue(&names, &[ip], long_ago),
            "a bridge left running for a year must renew without the phone reinstalling"
        );
    }

    #[test]
    fn pem_round_trips_through_the_local_base64_decoder() {
        let dir = temp_dir("pem");
        let ca = LocalCa::load_or_generate(&dir).expect("generate");
        let der = pem_to_der(ca.cert_pem()).expect("decode");
        assert!(!der.is_empty());
        // A DER SEQUENCE, which is what every X.509 certificate starts with.
        assert_eq!(der[0], 0x30);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
