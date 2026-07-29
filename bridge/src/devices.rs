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

use crate::{log_info, log_warn};

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
    /// Load the credential file, distinguishing **"no file"** from
    /// **"unreadable file"**.
    ///
    /// These are not the same event and must not produce the same silence. No
    /// file is a first run. An unreadable file is every paired device being
    /// un-paired at once — and the phone's symptom is a refused socket, close
    /// 1006, "bridge unreachable", which is precisely the misattribution this
    /// whole change exists to remove.
    ///
    /// `LocalCa::load_or_generate` already gives losing the CA this treatment.
    /// This is the same class of loss and it was being swallowed by an
    /// `.ok().unwrap_or_default()` chain.
    ///
    /// It still starts empty, because refusing to run would be worse — but the
    /// operator is told, and the damaged file is kept rather than overwritten
    /// so it can be inspected.
    pub fn load(path: PathBuf) -> Self {
        let devices = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<DeviceFile>(&text) {
                Ok(file) => file
                    .devices
                    .into_iter()
                    .map(|d| (d.id.clone(), d))
                    .collect(),
                Err(e) => {
                    // Keep the damaged file. The next write renames over it, so
                    // copy it aside first — this is the only evidence of what
                    // the devices were.
                    let salvage = path.with_extension("corrupt");
                    let saved = std::fs::copy(&path, &salvage).is_ok();
                    log_warn!(
                        "[devices] {} could not be parsed ({e}), so NO devices are paired and \
                         every phone must pair again. {} \
                         A phone in this state reports the bridge as unreachable; it is not.",
                        path.display(),
                        if saved {
                            format!("The damaged file was copied to {}.", salvage.display())
                        } else {
                            "The damaged file could not be copied aside.".to_string()
                        }
                    );
                    HashMap::new()
                }
            },
            // A missing file is an ordinary first run and says nothing.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => {
                log_warn!(
                    "[devices] {} could not be read ({e}), so no devices are paired and every \
                     phone must pair again.",
                    path.display()
                );
                HashMap::new()
            }
        };
        Self {
            path,
            devices: RwLock::new(devices),
        }
    }

    /// Exchange a valid pairing token for a new device credential.
    ///
    /// Returns the cookie value — `<id>.<secret>`. The secret is **never
    /// stored**: only its SHA-256 hash is written to disk, it is never logged,
    /// and it cannot be recovered from the credential file. A device that loses
    /// it pairs again.
    ///
    /// **That is a claim about the stored form, not about the secret's
    /// lifetime, and the distinction matters.** This doc previously said
    /// returning it was "the only moment the secret exists outside the phone",
    /// which is false in the direction that misleads: the secret is created
    /// here, travels to the phone once in `Set-Cookie`, and then comes back
    /// **on every single request** in the `Cookie` header. It is in flight
    /// constantly and in this process's memory on every `verify`.
    ///
    /// Anyone reasoning about exposure from the old sentence would conclude the
    /// secret crosses the wire once. It crosses continuously — which is exactly
    /// why `Secure`, `HttpOnly` and the `Origin` requirement on the cookie path
    /// are load-bearing rather than belt-and-braces.
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

        self.commit(|devices| {
            devices.insert(stored.id.clone(), stored.clone());
        })?;

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
        let found = self.commit(|devices| match devices.get_mut(id) {
            Some(device) => {
                device.label = label;
                true
            }
            None => false,
        })?;
        if found {
            Ok(())
        } else {
            Err(format!("no device {id}"))
        }
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
        let removed = self.commit(|devices| devices.remove(id).is_some())?;
        if removed {
            log_info!("[devices] revoked {id}");
        }
        Ok(removed)
    }

    /// Un-pair every device at once.
    ///
    /// # Why this has to exist next to token rotation
    ///
    /// Rotating the pairing token stops the *token* being usable. It does
    /// **nothing** to credentials already issued from it — verified: a cookie
    /// obtained before a rotation still authorises afterwards.
    ///
    /// That matters because of how the token is exposed. The QR necessarily
    /// points at plain HTTP, so anyone on the LAN at that moment can read it.
    /// Before per-device credentials, rotating closed that window. Now an
    /// eavesdropper who catches the token can exchange it once for a credential
    /// of their own, and **rotation does not reach it**. Rotation stops future
    /// pairings; only this stops the ones already made.
    ///
    /// So the honest pair of actions is: *rotate* to invalidate the QR, and
    /// *revoke all* to invalidate what the QR has already produced. Neither
    /// implies the other, and a UI offering only rotation promises something it
    /// does not deliver.
    ///
    /// Like [`Self::revoke`], this is only half. **The caller must also close
    /// the live sockets** — `ClientRegistry::revoke` for each returned id.
    pub fn revoke_all(&self) -> Result<Vec<String>, String> {
        let ids = self.commit(|devices| {
            let ids: Vec<String> = devices.keys().cloned().collect();
            devices.clear();
            ids
        })?;
        if !ids.is_empty() {
            log_info!("[devices] revoked all {} paired device(s)", ids.len());
        }
        Ok(ids)
    }

    /// Change the credential set: write the new state to disk, **then** commit
    /// it to memory.
    ///
    /// The order is the whole point, and it was wrong before.
    ///
    /// Mutating the map first and persisting afterwards means that when the
    /// write fails, memory and disk disagree — and memory is what every check
    /// consults. For `revoke` that is a safety defect rather than an
    /// inconsistency: the device disappears from the panel, the user is told it
    /// is gone, and the next restart reloads it from the file and lets it back
    /// in. The user has no reason to look again.
    ///
    /// Committing to memory only after the bytes are on disk means the failure
    /// mode is "the revoke did not happen and said so", which is recoverable by
    /// pressing the button again.
    ///
    /// The write lock is held across the file write. That is deliberate: it is
    /// a few hundred bytes, and it makes the disk write and the memory swap one
    /// atomic step rather than two that a concurrent caller can interleave.
    fn commit<T>(
        &self,
        change: impl FnOnce(&mut HashMap<String, StoredDevice>) -> T,
    ) -> Result<T, String> {
        let mut devices = self.devices.write().map_err(|_| "device store poisoned")?;
        let mut next = devices.clone();
        let outcome = change(&mut next);
        self.persist_map(&next)?;
        *devices = next;
        Ok(outcome)
    }

    /// Write a credential set to disk under a temporary file and rename.
    fn persist_map(&self, devices: &HashMap<String, StoredDevice>) -> Result<(), String> {
        let file = DeviceFile {
            devices: devices.values().cloned().collect(),
        };
        let text = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;

        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Unique **per call**, not per process. `process::id()` alone gave every
        // concurrent writer in this process the same temp path, so two threads
        // wrote the same file and both renamed it — measured as half of four
        // concurrent mints failing, and at 64 the file left as invalid JSON
        // with every credential lost. A counter is what makes the name unique;
        // the pid only separates processes sharing a directory.
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = self
            .path
            .with_extension(format!("tmp{}.{seq}", std::process::id()));
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
    fn concurrent_pairings_all_survive_a_reload() {
        // Measured before the fix: at four-way concurrency half the mints
        // returned Err; at 64, of 60 cookies handed out **none** survived a
        // reload and the file was left as invalid JSON. Two causes — the
        // snapshot and the replace were not one step, and every concurrent
        // writer in the process built the *same* temp path from `process::id()`
        // alone, so threads wrote one file and both renamed it.
        //
        // Not exotic: the clients panel drives `set_label` and `revoke` from the
        // UI while sockets are pairing.
        let s = std::sync::Arc::new(store("concurrent"));
        let mut handles = Vec::new();
        for _ in 0..16 {
            let s = std::sync::Arc::clone(&s);
            handles.push(std::thread::spawn(move || s.mint().expect("mint must not fail under concurrency")));
        }
        let cookies: Vec<String> = handles
            .into_iter()
            .map(|h| h.join().expect("thread").0)
            .collect();

        assert_eq!(cookies.len(), 16);
        assert_eq!(s.list().len(), 16, "every mint must be in the store");

        // The real assertion: a credential handed to a phone must still work
        // after a restart. Anything less means a device that paired
        // successfully is told to pair again.
        let path = s.path.clone();
        drop(s);
        let reloaded = DeviceStore::load(path.clone());
        for cookie in &cookies {
            let header = format!("{COOKIE_NAME}={cookie}");
            assert!(
                reloaded.verify(Some(&header)).is_some(),
                "a credential issued to a phone must survive a reload"
            );
        }

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_unreadable_file_is_reported_rather_than_silently_emptied() {
        // A corrupt file un-pairs every device at once. Starting empty is the
        // right behaviour — refusing to run would be worse — but doing it
        // silently means the operator's only clue is a phone reporting the
        // bridge as unreachable, which is the misattribution this whole change
        // exists to remove.
        let path = std::env::temp_dir().join(format!(
            "coyote-bridge-devices-corrupt-{}.json",
            std::process::id()
        ));
        std::fs::write(&path, "{ not valid json").expect("write");

        let s = DeviceStore::load(path.clone());
        assert!(s.list().is_empty());
        // The damaged file is preserved, because the next write renames over it
        // and it is the only evidence of what was paired.
        assert!(
            path.with_extension("corrupt").exists(),
            "the unreadable file must be kept for inspection"
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("corrupt"));
    }

    #[test]
    fn a_revoke_that_cannot_be_written_does_not_take_effect_in_memory() {
        // The order matters and it used to be wrong. Mutating memory first and
        // persisting after means a failed write leaves the two disagreeing —
        // and memory is what every check consults. For revoke that is a safety
        // defect, not an inconsistency: the device vanishes from the panel, the
        // user is told it is gone, and the next restart reloads it from the
        // file and lets it back in. Nobody looks twice.
        //
        // Failing loudly and changing nothing is recoverable; succeeding
        // visibly and changing nothing durable is not.
        let s = store("revoke-write-fails");
        let (cookie, device) = s.mint().expect("mint");

        // Make the write fail: replace the file's parent with something that
        // cannot hold it. A directory where the file should be does the job on
        // both platforms — the rename cannot clobber it.
        std::fs::remove_file(&s.path).ok();
        std::fs::create_dir_all(&s.path).expect("occupy the path with a directory");

        let result = s.revoke(&device.id);
        assert!(result.is_err(), "an unwritable store must report failure");

        // And the credential must still work, because nothing durable changed.
        // A device that stops being honoured while the record survives on disk
        // is the same divergence pointing the other way.
        let header = format!("{COOKIE_NAME}={cookie}");
        assert!(
            s.verify(Some(&header)).is_some(),
            "a failed revoke must leave the device exactly as it was"
        );

        let _ = std::fs::remove_dir_all(&s.path);
    }

    #[test]
    fn revoke_all_reaches_what_token_rotation_cannot() {
        // Rotation invalidates the QR. It does nothing to credentials already
        // exchanged from it — so an eavesdropper who caught the token on the
        // plain-HTTP first hop keeps access that rotation cannot touch. This is
        // the action that reaches them.
        let s = store("revoke-all");
        let (a, _) = s.mint().expect("a");
        let (b, _) = s.mint().expect("b");

        let ids = s.revoke_all().expect("revoke all");
        assert_eq!(ids.len(), 2, "the caller needs every id, to close every socket");
        assert!(s.list().is_empty());

        for cookie in [&a, &b] {
            let header = format!("{COOKIE_NAME}={cookie}");
            assert!(s.verify(Some(&header)).is_none());
        }

        let _ = std::fs::remove_file(&s.path);
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
