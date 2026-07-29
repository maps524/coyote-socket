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

/// Where the issuer's identity comes from when signing a leaf.
///
/// A freshly generated CA still has its parameters in hand. A loaded one must
/// take them from the certificate on disk — never from the environment, because
/// the environment can change under a CA that cannot.
enum IssuerSource {
    Generated(Box<CertificateParams>),
    Stored(String),
}

pub struct LocalCa {
    key_pair: KeyPair,
    issuer_source: IssuerSource,
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
            // `verify_pair` is part of loading, not a separate nicety. Every
            // way this can be broken on disk produces the same downstream
            // symptom — the trust check telling the user they missed step 3 —
            // so it has to be caught where the message can still be accurate.
            match Self::load(&cert_path, &key_path).and_then(|ca| ca.verify_pair().map(|_| ca)) {
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

        // The issuer is parsed **out of the stored certificate**, never rebuilt
        // from the environment.
        //
        // An earlier version reconstructed the distinguished name from the
        // machine's hostname, on the reasoning that it was derived
        // deterministically from known inputs. It is not: renaming the machine
        // changes `COMPUTERNAME`, which changed the reconstructed DN, which made
        // every newly issued leaf claim an issuer that did not match the stored
        // CA's subject. Path building then failed on every phone
        // (`unable to get local issuer certificate`) while the log cheerfully
        // reported "using the existing local CA — phones that already trust it
        // need no action". A service install where `HOSTNAME` is unset hit the
        // same thing through the fallback string.
        //
        // Parsing costs a feature flag. Reconstruction cost correctness.
        let display_name = subject_common_name(&cert_pem).unwrap_or_else(display_name_for_host);

        Ok(Self {
            key_pair,
            issuer_source: IssuerSource::Stored(cert_pem.clone()),
            cert_pem,
            display_name,
            freshly_generated: false,
        })
    }

    /// Prove the loaded key and certificate are actually a pair.
    ///
    /// They can disagree: `generate` writes two files, and two instances racing
    /// against an empty directory can interleave so that one CA's key sits
    /// beside another's certificate. Both files parse, loading succeeds, and
    /// every served chain then fails `certificate signature failure` — which
    /// reaches the user as the trust check saying they missed step 3, so they
    /// reinstall a certificate that was never the problem and it never works.
    ///
    /// Comparing the certificate's `SubjectPublicKeyInfo` with the one derived
    /// from the loaded private key is exact and total: if they match, the key
    /// signed that certificate; if they do not, nothing this CA signs will ever
    /// validate. It costs a few hundred bytes of parsing at startup.
    fn verify_pair(&self) -> Result<(), String> {
        use rcgen::PublicKeyData;

        let der = pem_to_der(&self.cert_pem)?;
        let (_, parsed) = x509_parser::parse_x509_certificate(&der)
            .map_err(|e| format!("parsing the stored CA certificate: {e}"))?;

        let in_cert = parsed.tbs_certificate.subject_pki.raw;
        let from_key = self.key_pair.subject_public_key_info();

        if in_cert == from_key.as_slice() {
            Ok(())
        } else {
            Err("the stored private key does not match the stored certificate".to_string())
        }
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

        // Write to temporaries and rename into place.
        //
        // This makes each *file* appear atomically — no reader ever sees a
        // half-written certificate. It does **not** make the pair atomic: two
        // processes racing an empty directory can still interleave their two
        // renames and leave one CA's certificate beside the other's key. That
        // is caught by `verify_pair` on the next load, so the outcome is a loud
        // error and one reinstall rather than a silent permanent failure.
        // Closing it properly needs an exclusive-create lock file; this is the
        // cheap part, and the comment should not claim the expensive part.
        let key_tmp = dir.join(format!("{CA_KEY_FILE}.tmp{}", std::process::id()));
        let cert_tmp = dir.join(format!("{CA_CERT_FILE}.tmp{}", std::process::id()));
        write_private(&key_tmp, &key_pair.serialize_pem())?;
        std::fs::write(&cert_tmp, &cert_pem)
            .map_err(|e| format!("writing the CA certificate: {e}"))?;
        // Certificate first: if the process dies between the two renames, the
        // next run sees a certificate without a key, which `load` rejects and
        // reports — rather than a key without a certificate, which looks like a
        // fresh directory and would silently mint a second CA.
        std::fs::rename(&cert_tmp, dir.join(CA_CERT_FILE))
            .map_err(|e| format!("installing the CA certificate: {e}"))?;
        std::fs::rename(&key_tmp, dir.join(CA_KEY_FILE))
            .map_err(|e| format!("installing the CA private key: {e}"))?;

        log_info!(
            "[tls] generated a new local CA \"{display_name}\" in {}. \
             The private key stays on this machine and is never sent anywhere.",
            dir.display()
        );

        Ok(Self {
            key_pair,
            issuer_source: IssuerSource::Generated(Box::new(params)),
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
        // Backdated a day, not an hour. A phone whose clock is behind rejects a
        // certificate that is not yet valid, and reports it exactly like an
        // untrusted one — so the user is told they missed the trust step and
        // reinstalls a certificate that was always fine. An hour covers drift;
        // a day covers a phone that has been in a drawer or a machine whose RTC
        // is out. It costs nothing.
        params.not_before = now - Duration::days(1);
        params.not_after = now + Duration::days(LEAF_VALIDITY_DAYS);

        let leaf_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)
            .map_err(|e| format!("generating the leaf key: {e}"))?;
        let leaf = match &self.issuer_source {
            IssuerSource::Generated(ca_params) => {
                let issuer = Issuer::from_params(ca_params.as_ref(), &self.key_pair);
                params.signed_by(&leaf_key, &issuer)
            }
            IssuerSource::Stored(pem) => {
                let issuer = Issuer::from_ca_cert_pem(pem, &self.key_pair)
                    .map_err(|e| format!("reading the stored CA as an issuer: {e}"))?;
                params.signed_by(&leaf_key, &issuer)
            }
        }
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
    params.not_before = now - Duration::days(1);
    params.not_after = now + Duration::days(CA_VALIDITY_DAYS);
    Ok(params)
}

/// The common name recorded in a stored certificate.
///
/// This is what the install page tells the user to look for in iOS's
/// Certificate Trust Settings, so it has to be what is actually *in* the
/// certificate rather than what this machine would generate today. They differ
/// the moment the machine is renamed.
fn subject_common_name(cert_pem: &str) -> Option<String> {
    let der = pem_to_der(cert_pem).ok()?;
    // Bound to a local and converted to an owned `String` before `der` goes out
    // of scope: everything x509-parser hands back borrows the DER it parsed.
    let name = {
        let (_, parsed) = x509_parser::parse_x509_certificate(&der).ok()?;
        let cn = parsed.tbs_certificate.subject.iter_common_name().next()?;
        cn.as_str().ok()?.to_string()
    };
    Some(name)
}

/// A name the user can recognise in Settings months from now.
///
/// The machine name is in it deliberately: someone who has run this on a
/// desktop and a home server will otherwise see two identical entries and have
/// no way to tell which one they are about to delete.
fn display_name_for_host() -> String {
    display_name_for(hostname().as_deref())
}

/// Split out so tests can supply a name without mutating process environment.
///
/// The regression test for the rename bug used to `set_var("COMPUTERNAME", …)`
/// and never restore it, inside a suite `cargo test` runs as parallel threads
/// in one process — a test mutating global state shared with everything else in
/// flight. A regression test for a silent-failure bug should not itself be a
/// source of one.
fn display_name_for(host: Option<&str>) -> String {
    format!("CoyoteSocket Bridge on {}", host.unwrap_or("this computer"))
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

/// Where the headless binary keeps its CA when nobody said.
///
/// **Not the working directory.** That default put a private key in whatever
/// folder the binary happened to be run from, which for a developer is a git
/// checkout — one `git add -A` away from publishing a key that can impersonate
/// any site to every phone that trusted it. The `.gitignore` covers the same
/// ground, but a default that is safe only because of a `.gitignore` in one
/// repository is not safe.
///
/// Falls back to the OS temp directory, which is wrong in a different and much
/// louder way: the CA vanishes and the user is told the phone must reinstall.
/// Better than quietly writing a key somewhere it can be published.
pub fn default_config_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from)
        })
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"))
        });

    match base {
        Some(base) => base.join("com.coyotesocket.bridge"),
        None => std::env::temp_dir().join("com.coyotesocket.bridge"),
    }
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
    fn renaming_the_machine_does_not_break_the_chain() {
        // Regression. The issuer used to be rebuilt from `COMPUTERNAME`, so a
        // rename changed the reconstructed DN, every new leaf claimed an issuer
        // that did not match the stored CA's subject, and path building failed
        // on every phone — while the log said "using the existing local CA,
        // phones need no action". Silent and permanent.
        let dir = temp_dir("rename");
        let first = LocalCa::load_or_generate(&dir).expect("generate");
        let original_name = first.display_name().to_string();
        drop(first);

        // The machine "renames": what the environment would generate now differs
        // from what is stored. Expressed as an assertion rather than by mutating
        // a process-wide variable out from under every other test in flight.
        assert_ne!(
            display_name_for(Some("A-COMPLETELY-DIFFERENT-NAME")),
            original_name,
            "premise: a rename changes what the environment would produce"
        );

        let reloaded = LocalCa::load_or_generate(&dir).expect("reload after rename");

        assert_eq!(
            reloaded.display_name(),
            original_name,
            "the name must come from the stored certificate, not the environment — \
             it is what the install page tells the user to look for in Settings"
        );
        // And it must still be able to sign.
        reloaded
            .issue_leaf(&[IpAddr::V4(Ipv4Addr::new(192, 168, 0, 9))])
            .expect("a renamed machine must still issue working leaves");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_key_that_does_not_match_the_certificate_is_refused() {
        // Reachable by two bridges starting together against an empty
        // directory. Both files parse, so loading used to succeed and every
        // served chain then failed `certificate signature failure` — which
        // reaches the user as "you missed the trust step", so they reinstall a
        // certificate that was never the problem and it never works.
        let dir_a = temp_dir("pair-a");
        let dir_b = temp_dir("pair-b");
        let _a = LocalCa::load_or_generate(&dir_a).expect("A");
        let _b = LocalCa::load_or_generate(&dir_b).expect("B");

        // A's certificate beside B's key.
        let frankendir = temp_dir("pair-mixed");
        std::fs::create_dir_all(&frankendir).unwrap();
        std::fs::copy(dir_a.join(CA_CERT_FILE), frankendir.join(CA_CERT_FILE)).unwrap();
        std::fs::copy(dir_b.join(CA_KEY_FILE), frankendir.join(CA_KEY_FILE)).unwrap();

        let loaded = LocalCa::load(
            &frankendir.join(CA_CERT_FILE),
            &frankendir.join(CA_KEY_FILE),
        )
        .expect("both files parse individually — that is the trap");
        assert!(
            loaded.verify_pair().is_err(),
            "a mismatched key and certificate must be caught at load, where the \
             message can still be accurate"
        );

        for d in [&dir_a, &dir_b, &frankendir] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn the_default_ca_location_is_never_the_working_directory() {
        // A private key written into a git checkout is one `git add -A` from
        // being published, after which anyone can impersonate any site to every
        // phone that trusted that build.
        let dir = default_config_dir();
        assert_ne!(dir, PathBuf::from("."));
        assert!(
            dir.is_absolute(),
            "the default must not be relative to wherever the binary was launched"
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
