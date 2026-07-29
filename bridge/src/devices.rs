//! Per-device credentials: pair once, then never present the token again.
//!
//! # What this does not give you
//!
//! Stated first, in the habit `auth.rs` set, because the failure mode of an
//! auth mechanism is someone downstream believing it covers more than it does.
//!
//! - **A cookie is still a bearer credential.** Whoever holds it is the device.
//!   It is not a signature, not a challenge-response, and there is no proof of
//!   possession beyond presenting the bytes.
//! - **It is not identity.** A credential identifies **a browser storage
//!   partition**, not a handset and not a person. Two browsers on one phone are
//!   two devices here and always will be. A phone that clears its cookies is a
//!   new device with no way to know it was the old one.
//! - **It is not a defence against filesystem access.** The secrets are stored
//!   hashed, which stops a leaked settings file from being replayed as a
//!   cookie — but anyone who can write this file can mint themselves a device.
//!
//! # What it does give you, that a single shared token did not
//!
//! **Revocation that means something.** With one token, revoking is
//! all-or-nothing: rotate it and every paired device stops, which is exactly
//! why auto-rotation was rejected — pairing a tablet would silently un-pair the
//! phone. Per-device credentials make revoke mean *"stop trusting the tablet"*.
//!
//! And it gives the connected-clients panel a real identity, which an address
//! cannot supply: an address cannot tell a second phone from the same phone
//! that roamed onto a different network.
//!
//! # Why a cookie rather than `localStorage`
//!
//! The deciding reason is the WebSocket upgrade. **Cookies ride it
//! automatically**; `localStorage` does not, so every socket would need
//! JavaScript to read the value and append it to the URL — a second code path
//! that can be got wrong, in the one place where being wrong surfaces as close
//! code 1006 and reads to the user as "bridge unreachable".
//!
//! `HttpOnly` then comes free, and it is most of the value: no script can read
//! the credential, so a compromised app page cannot exfiltrate it. `Secure` and
//! `SameSite` come free too. `localStorage` offers none of the three.
//!
//! # The origin problem this solves
//!
//! Pairing happens on `http://host:8787` and the app runs on
//! `https://host:8443`. **Different scheme and different port means different
//! origin, so nothing in browser storage crosses** — which is why an earlier
//! design that expected the token to survive in `localStorage` failed with the
//! app reporting "bridge unreachable".
//!
//! The token now crosses that boundary exactly once, on the URL, in the flow
//! built for it: the install page hands off to the HTTPS origin, and *that
//! origin* exchanges the token for a cookie. One trip, deliberately, instead of
//! an expectation that storage will do something it cannot.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

use crate::log_info;

/// The cookie the app presents on every request and every socket upgrade.
pub const COOKIE_NAME: &str = "coyote_device";

/// Bytes in the public device id. Not a secret — it is rendered in the clients
/// panel, appears in `/healthz`, and lands in bug reports. It is generated
/// independently of the secret so that publishing it reveals nothing.
const ID_BYTES: usize = 16;

/// Bytes in the bearer secret.
const SECRET_BYTES: usize = 32;

/// Cookie lifetime. Long, because the entire point is that the home-screen
/// shortcut keeps working without ceremony; expiry would reintroduce the
/// re-pairing this exists to remove.
const COOKIE_MAX_AGE_SECS: u64 = 400 * 24 * 60 * 60;

/// What the rest of the bridge learns from a valid cookie.
///
/// Carries no secret, so it is safe to render, log and serialise.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedDevice {
    pub id: String,
    pub label: Option<String>,
    /// When this device first paired. Durable across restarts, which a
    /// per-run registry cannot be.
    pub created_ms: u64,
}

/// One stored credential.
///
/// The secret is stored **hashed**. A leaked settings file then cannot be
/// replayed as a cookie, which matters because that file is the same one users
/// are asked to paste when reporting problems.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredDevice {
    id: String,
    secret_sha256: String,
    created_ms: u64,
    #[serde(default)]
    label: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeviceFile {
    #[serde(default)]
    devices: Vec<StoredDevice>,
}

/// The credential store, persisted beside the settings file.
pub struct DeviceStore {
    path: PathBuf,
    devices: RwLock<HashMap<String, StoredDevice>>,
}

impl DeviceStore {
    pub fn load(path: PathBuf) -> Self {
        let devices = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<DeviceFile>(&text).ok())
            .map(|file| {
                file.devices
                    .into_iter()
                    .map(|d| (d.id.clone(), d))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            path,
            devices: RwLock::new(devices),
        }
    }

    /// Exchange a valid pairing token for a new device credential.
    ///
    /// Returns the cookie value — `<id>.<secret>` — which is the only moment
    /// the secret exists outside the phone. It is not stored, not logged and
    /// not recoverable; a device that loses it pairs again.
    pub fn mint(&self) -> Result<(String, VerifiedDevice), String> {
        let id = random_hex(ID_BYTES)?;
        let secret = random_hex(SECRET_BYTES)?;
        let created_ms = now_ms();

        let stored = StoredDevice {
            id: id.clone(),
            secret_sha256: sha256_hex(secret.as_bytes()),
            created_ms,
            label: None,
        };

        {
            let mut devices = self.devices.write().map_err(|_| "device store poisoned")?;
            devices.insert(id.clone(), stored);
        }
        self.persist()?;

        log_info!("[devices] paired a new device ({id})");
        Ok((
            format!("{id}.{secret}"),
            VerifiedDevice {
                id,
                label: None,
                created_ms,
            },
        ))
    }

    /// Verify a `Cookie:` header and return the device it identifies.
    pub fn verify(&self, cookie_header: Option<&str>) -> Option<VerifiedDevice> {
        let value = cookie_value(cookie_header?, COOKIE_NAME)?;
        let (id, secret) = value.split_once('.')?;

        let devices = self.devices.read().ok()?;
        let stored = devices.get(id)?;

        // Constant-time, for the same reason `Token::matches` is: the cost is
        // four lines and "probably not exploitable over a LAN" is a poor thing
        // to have to re-evaluate later.
        if !constant_time_eq(stored.secret_sha256.as_bytes(), sha256_hex(secret.as_bytes()).as_bytes()) {
            return None;
        }

        Some(VerifiedDevice {
            id: stored.id.clone(),
            label: stored.label.clone(),
            created_ms: stored.created_ms,
        })
    }

    /// Every paired device, for the clients panel.
    pub fn list(&self) -> Vec<VerifiedDevice> {
        let Ok(devices) = self.devices.read() else {
            return Vec::new();
        };
        let mut out: Vec<VerifiedDevice> = devices
            .values()
            .map(|d| VerifiedDevice {
                id: d.id.clone(),
                label: d.label.clone(),
                created_ms: d.created_ms,
            })
            .collect();
        // Stable order, oldest first, so the panel does not reshuffle.
        out.sort_by_key(|d| (d.created_ms, d.id.clone()));
        out
    }

    /// Name a device. `label` is expected to be sanitised by the caller.
    pub fn set_label(&self, id: &str, label: Option<String>) -> Result<(), String> {
        {
            let mut devices = self.devices.write().map_err(|_| "device store poisoned")?;
            let Some(device) = devices.get_mut(id) else {
                return Err(format!("no device {id}"));
            };
            device.label = label;
        }
        self.persist()
    }

    /// Delete a credential so it can never be presented **again**.
    ///
    /// # This is half of revoking a device. On its own it is a safety defect.
    ///
    /// Deleting the record stops the *next* connection. It does nothing to the
    /// socket that is open right now, and that socket is what is driving
    /// hardware. A caller that stops here produces the worst available outcome:
    /// the record is gone, the panel says the device is revoked, the user
    /// believes they have disconnected it — and the phone keeps its relay and
    /// keeps driving output until the network happens to drop it.
    ///
    /// That is the shape this project has already fixed twice elsewhere: **a
    /// state that reads as safe while the thing it describes is still live.**
    ///
    /// So every caller must also close the live sockets:
    ///
    /// ```ignore
    /// state.devices.revoke(&id)?;              // durable before we go on
    /// let closed = state.clients.revoke(&id);  // and stop what is running now
    /// ```
    ///
    /// Store first is deliberate: a socket that reconnects in the gap between
    /// the two is then refused rather than re-admitted.
    ///
    /// The ordering cannot be enforced from here — `clients` is a different
    /// module and this one deliberately does not depend on it — so it is
    /// enforced by saying so at the only place a caller will look.
    pub fn revoke(&self, id: &str) -> Result<bool, String> {
        let removed = {
            let mut devices = self.devices.write().map_err(|_| "device store poisoned")?;
            devices.remove(id).is_some()
        };
        if removed {
            // Persist before returning. A revoke that is not on disk when the
            // process dies is a device that comes back from the dead — the same
            // shape as the settings race, and worse, because the user believes
            // they removed it.
            self.persist()?;
            log_info!("[devices] revoked {id}");
        }
        Ok(removed)
    }

    /// Write the whole list under a temporary file and rename.
    ///
    /// Read-modify-write of an in-memory map under a lock, then one atomic
    /// replace — so a concurrent writer cannot interleave and resurrect an
    /// entry that was just revoked.
    fn persist(&self) -> Result<(), String> {
        let devices = self.devices.read().map_err(|_| "device store poisoned")?;
        let file = DeviceFile {
            devices: devices.values().cloned().collect(),
        };
        let text = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
        drop(devices);

        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = self.path.with_extension(format!("tmp{}", std::process::id()));
        std::fs::write(&tmp, text).map_err(|e| format!("writing devices: {e}"))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| format!("installing devices: {e}"))?;
        Ok(())
    }
}

/// The `Set-Cookie` header value for a freshly minted credential.
///
/// `HttpOnly` is the important one: no script can read this, so a compromised
/// app page cannot exfiltrate the credential. `Secure` because it is only ever
/// set on the TLS origin. `SameSite=Lax` rather than `Strict` so that following
/// the install page's link into the app still sends it.
pub fn set_cookie_header(value: &str) -> String {
    format!(
        "Set-Cookie: {COOKIE_NAME}={value}; Path=/; Max-Age={COOKIE_MAX_AGE_SECS}; \
         Secure; HttpOnly; SameSite=Lax\r\n"
    )
}

/// The public id half of a cookie, without verifying it.
///
/// For callers that only need something to key on and will verify separately.
pub fn id_from_cookie_header(cookie_header: Option<&str>) -> Option<&str> {
    let value = cookie_value(cookie_header?, COOKIE_NAME)?;
    Some(value.split_once('.').map(|(id, _)| id).unwrap_or(value))
}

/// Pull one cookie out of a `Cookie:` header.
fn cookie_value<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header.split(';').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key.trim() == name).then_some(value.trim())
    })
}

fn random_hex(bytes: usize) -> Result<String, String> {
    let mut buf = vec![0u8; bytes];
    getrandom::getrandom(&mut buf).map_err(|e| format!("no randomness available: {e}"))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

fn sha256_hex(input: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, input);
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut difference = 0u8;
    for (x, y) in a.iter().zip(b) {
        difference |= x ^ y;
    }
    difference == 0
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Where the credential file lives — beside the settings, one place to back up.
pub fn devices_path(config_dir: &Path) -> PathBuf {
    config_dir.join("bridge-devices.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(tag: &str) -> DeviceStore {
        let path = std::env::temp_dir().join(format!(
            "coyote-bridge-devices-{tag}-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        DeviceStore::load(path)
    }

    #[test]
    fn a_minted_cookie_verifies_and_a_forged_one_does_not() {
        let s = store("mint");
        let (cookie, device) = s.mint().expect("mint");

        let header = format!("{COOKIE_NAME}={cookie}");
        let verified = s.verify(Some(&header)).expect("verify");
        assert_eq!(verified.id, device.id);

        // Right id, wrong secret. The id is public, so this is the attack that
        // matters: knowing an id must not be enough.
        let forged = format!("{COOKIE_NAME}={}.{}", device.id, "0".repeat(64));
        assert!(s.verify(Some(&forged)).is_none());

        let _ = std::fs::remove_file(&s.path);
    }

    #[test]
    fn the_secret_is_never_stored_in_recoverable_form() {
        // The store sits next to the log and the settings, which are exactly
        // the files users paste into bug reports.
        let s = store("hashing");
        let (cookie, _) = s.mint().expect("mint");
        let secret = cookie.split_once('.').unwrap().1;

        let on_disk = std::fs::read_to_string(&s.path).expect("persisted");
        assert!(
            !on_disk.contains(secret),
            "the bearer secret must not be recoverable from the credential file"
        );

        let _ = std::fs::remove_file(&s.path);
    }

    #[test]
    fn the_public_id_has_no_relationship_to_the_secret() {
        // The id is rendered in the clients panel and lands in bug reports, so
        // it must not be derived from the secret in any way.
        let s = store("id");
        let (cookie, device) = s.mint().expect("mint");
        let secret = cookie.split_once('.').unwrap().1;
        assert!(!secret.contains(&device.id));
        assert!(!sha256_hex(secret.as_bytes()).contains(&device.id));

        let _ = std::fs::remove_file(&s.path);
    }

    #[test]
    fn credentials_survive_a_restart() {
        // The whole point: the home-screen shortcut keeps working without
        // re-pairing.
        let s = store("restart");
        let (cookie, device) = s.mint().expect("mint");
        let path = s.path.clone();
        drop(s);

        let reloaded = DeviceStore::load(path.clone());
        let header = format!("{COOKIE_NAME}={cookie}");
        let verified = reloaded.verify(Some(&header)).expect("still valid");
        assert_eq!(verified.id, device.id);
        assert_eq!(verified.created_ms, device.created_ms);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn revoking_one_device_leaves_the_others_alone() {
        // The entire reason this replaced a single shared token: revoke must
        // mean "stop trusting the tablet", not "un-pair everything".
        let s = store("revoke");
        let (cookie_a, a) = s.mint().expect("a");
        let (cookie_b, _) = s.mint().expect("b");

        assert!(s.revoke(&a.id).expect("revoke"), "should report a removal");

        let header_a = format!("{COOKIE_NAME}={cookie_a}");
        let header_b = format!("{COOKIE_NAME}={cookie_b}");
        assert!(s.verify(Some(&header_a)).is_none(), "revoked device must be refused");
        assert!(s.verify(Some(&header_b)).is_some(), "the other device must be unaffected");

        let _ = std::fs::remove_file(&s.path);
    }

    #[test]
    fn a_revocation_is_on_disk_before_it_is_reported() {
        // A revoke that is not persisted is a device that comes back from the
        // dead — the same shape as the settings race, and worse, because the
        // user believes they removed it.
        let s = store("revoke-durable");
        let (cookie, device) = s.mint().expect("mint");
        s.revoke(&device.id).expect("revoke");
        let path = s.path.clone();
        drop(s);

        let reloaded = DeviceStore::load(path.clone());
        let header = format!("{COOKIE_NAME}={cookie}");
        assert!(
            reloaded.verify(Some(&header)).is_none(),
            "a revoked credential must not return after a restart"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn labels_are_stored_and_survive_a_restart() {
        let s = store("label");
        let (_, device) = s.mint().expect("mint");
        s.set_label(&device.id, Some("Sara's phone".into())).expect("label");
        let path = s.path.clone();
        drop(s);

        let reloaded = DeviceStore::load(path.clone());
        assert_eq!(
            reloaded.list().first().and_then(|d| d.label.clone()),
            Some("Sara's phone".into())
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_cookie_is_found_among_others() {
        // Browsers send every cookie for the origin in one header.
        let header = format!("other=1; {COOKIE_NAME}=abc.def; another=2");
        assert_eq!(cookie_value(&header, COOKIE_NAME), Some("abc.def"));
        assert_eq!(id_from_cookie_header(Some(&header)), Some("abc"));
    }

    #[test]
    fn a_missing_or_malformed_cookie_is_simply_absent() {
        let s = store("absent");
        assert!(s.verify(None).is_none());
        assert!(s.verify(Some("unrelated=1")).is_none());
        assert!(s.verify(Some(&format!("{COOKIE_NAME}=no-dot"))).is_none());
        let _ = std::fs::remove_file(&s.path);
    }

    #[test]
    fn the_list_is_stable_so_the_panel_does_not_reshuffle() {
        let s = store("order");
        let (_, a) = s.mint().expect("a");
        let (_, b) = s.mint().expect("b");
        let ids: Vec<String> = s.list().into_iter().map(|d| d.id).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&a.id) && ids.contains(&b.id));
        // Same answer twice.
        assert_eq!(ids, s.list().into_iter().map(|d| d.id).collect::<Vec<_>>());
        let _ = std::fs::remove_file(&s.path);
    }
}
