//! What the app remembers between runs.
//!
//! Only enough to make the second run one click: the last endpoint and a short
//! list of recent ones. The headset's IP moves on DHCP, so "pick from a list"
//! beats "retype it", and a list beats a single remembered value the first
//! time it changes and comes back.

use std::path::PathBuf;

use coyote_bridge::log_warn;
use serde::{Deserialize, Serialize};

/// How many endpoints to remember. Long enough to cover a headset, a desktop
/// player and the local fake; short enough to stay a menu rather than a
/// history.
const MAX_RECENTS: usize = 6;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Last endpoint connected to, `host:port`.
    pub endpoint: String,
    /// Most recent first, including `endpoint`.
    pub recents: Vec<String>,
    /// Port the bridge serves the phone app and the QR on.
    pub http_port: u16,
    /// Directory of static files to serve, when the PWA has been built.
    pub static_dir: Option<String>,

    /// The pairing token, persisted so the phone's home-screen shortcut keeps
    /// working across restarts.
    ///
    /// A per-run token would put a fresh secret in the URL on every launch and
    /// break that shortcut every time — the same churn that makes a rotating
    /// tunnel hostname unusable, relocated from the host into the query
    /// string. A stable origin with an unstable credential is not stable.
    ///
    /// It is stored in plaintext next to the log. That is the right level of
    /// protection for a LAN development tool and the wrong level for anything
    /// else, which is a fact worth stating rather than hiding: anyone who can
    /// read this file can drive the player.
    pub token: Option<String>,

    /// Whether *this* instance minted or rotated the token it holds.
    ///
    /// Not persisted — it is a claim about this process, not about the file.
    /// An instance that merely read a token at startup has no standing to
    /// write it back, because it may have been revoked since. See
    /// [`Settings::save`].
    #[serde(skip)]
    pub token_is_ours: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            recents: Vec::new(),
            http_port: 8787,
            static_dir: None,
            token: None,
            token_is_ours: false,
        }
    }
}

impl Settings {
    /// Record a successful (or attempted) endpoint, moving it to the front.
    pub fn remember(&mut self, endpoint: &str) {
        self.endpoint = endpoint.to_string();
        self.recents.retain(|e| e != endpoint);
        self.recents.insert(0, endpoint.to_string());
        self.recents.truncate(MAX_RECENTS);
    }

    /// The stored token, minting and storing one on first run.
    pub fn token(&mut self) -> coyote_bridge::auth::Token {
        match &self.token {
            Some(existing) => coyote_bridge::auth::Token::from_string(existing.clone()),
            None => {
                let fresh = coyote_bridge::auth::Token::generate();
                self.set_token(&fresh);
                fresh
            }
        }
    }

    /// Adopt `token` as this instance's, and claim the right to write it.
    ///
    /// The claim matters: see [`Settings::save`]. An instance that has not
    /// called this must never write a token, because the only token it has is
    /// one it read at startup and which may since have been revoked.
    pub fn set_token(&mut self, token: &coyote_bridge::auth::Token) {
        self.token = Some(token.as_str().to_string());
        self.token_is_ours = true;
    }

    pub fn load(path: &PathBuf) -> Self {
        // A missing or corrupt file is not worth surfacing: defaults are
        // perfectly usable and the alternative is an error dialog on launch
        // about something the user never asked for.
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Persist, without undoing another instance's work.
    ///
    /// # Why this is not `fs::write`
    ///
    /// It was, and that made the revoke button a lie. Two instances each hold
    /// their own `Settings` in memory and each wrote the whole struct. Instance
    /// A revokes a token it believes has leaked; instance B — which read the
    /// old token at startup and knows nothing about the revocation — later
    /// saves for an unrelated reason, a connect or a directory change, and
    /// writes the revoked token back. **The credential returns from the dead
    /// and nothing tells anyone.** Two instances is not exotic; it is the
    /// normal state of a machine that develops this app.
    ///
    /// Revocation that a race can undo is worse than no revoke button, on the
    /// same reasoning that retired the QR which advertised a revoked token: a
    /// control that reports success without delivering it is worse than an
    /// absent one, because it stops the user looking further.
    ///
    /// Three properties, each for a different failure:
    ///
    /// 1. **An exclusive advisory lock** around read-modify-write, so two
    ///    instances cannot interleave. An OS lock rather than a lockfile
    ///    sentinel, because the OS releases it when the handle closes —
    ///    including on a crash. A hand-rolled lockfile would trade this race
    ///    for a worse one, where a killed instance blocks saves forever.
    /// 2. **Read, merge, write** rather than overwrite. Fields this instance
    ///    owns are ours; the token is only ours if we minted or rotated it
    ///    (`token_is_ours`). Otherwise the on-disk token wins and we adopt it.
    ///    That is what makes resurrection impossible rather than unlikely.
    /// 3. **Temp file plus atomic rename**, so a process dying mid-write
    ///    leaves the previous settings intact instead of a truncated file that
    ///    parses as defaults — which would silently mint a *new* token and
    ///    un-pair every device.
    ///
    /// Failures are swallowed by design: a settings file that cannot be
    /// written must not take down a bridge that is otherwise working. They are
    /// logged.
    pub fn save(&mut self, path: &PathBuf) {
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                log_warn!("[settings] could not create {}: {e}", parent.display());
                return;
            }
        }

        let _guard = match SaveLock::acquire(path) {
            Some(guard) => guard,
            None => {
                // Better to write unlocked than not at all: the merge below
                // still prevents resurrection in every interleaving except a
                // microsecond-wide one, and refusing to save would lose the
                // user's endpoint.
                log_warn!("[settings] could not lock the settings file; saving unlocked");
                self.merge_and_write(path);
                return;
            }
        };
        self.merge_and_write(path);
    }

    /// Fold this instance's changes into whatever is currently on disk.
    fn merge_and_write(&mut self, path: &PathBuf) {
        let on_disk = Self::load(path);

        // The token is the field with a safety consequence, so it is the one
        // with an explicit owner. An instance that never touched it defers.
        if !self.token_is_ours {
            if let Some(theirs) = &on_disk.token {
                if self.token.as_deref() != Some(theirs.as_str()) {
                    log_warn!(
                        "[settings] adopting a token written by another instance; \
                         ours was stale and writing it back would have revived it"
                    );
                    self.token = Some(theirs.clone());
                }
            }
        }

        // **Keep this literal exhaustive. Do not "tidy" it into
        // `..self.clone()` or `..Default::default()`.**
        //
        // Naming every field is what forces a compile error when someone adds
        // one, and that error is the only thing standing between a new setting
        // and silent data loss: a struct-update fallthrough compiles happily
        // and resets the missing field on every save. The symptom then appears
        // somewhere else entirely — a port that reverts, a directory that
        // forgets — with nothing pointing back here.
        //
        // This has already fired once. `bridge-tls` added `https_port` and the
        // build stopped them rather than the port silently resetting to its
        // default on every write.
        let merged = Settings {
            endpoint: self.endpoint.clone(),
            recents: self.recents.clone(),
            http_port: self.http_port,
            static_dir: self.static_dir.clone(),
            token: self.token.clone(),
            token_is_ours: false,
        };

        let Ok(text) = serde_json::to_string_pretty(&merged) else {
            return;
        };

        // Same directory, so the rename is on one filesystem and therefore
        // atomic. A temp file elsewhere would degrade to copy-then-delete.
        let temp = path.with_extension("json.tmp");
        if let Err(e) = std::fs::write(&temp, text) {
            log_warn!("[settings] could not write {}: {e}", temp.display());
            return;
        }
        if let Err(e) = std::fs::rename(&temp, path) {
            log_warn!("[settings] could not replace {}: {e}", path.display());
            let _ = std::fs::remove_file(&temp);
        }
    }
}

/// An exclusive advisory lock held for the duration of a save.
///
/// Released when dropped, and by the OS if the process dies — which is the
/// whole reason for using a real lock rather than a sentinel file.
struct SaveLock(std::fs::File);

impl SaveLock {
    fn acquire(path: &PathBuf) -> Option<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path.with_extension("lock"))
            .ok()?;
        // `File::lock` is stable as of Rust 1.89, so this needs no crate.
        file.lock().ok()?;
        Some(Self(file))
    }
}

impl Drop for SaveLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembering_moves_an_endpoint_to_the_front_without_duplicating() {
        let mut s = Settings::default();
        s.remember("a:1");
        s.remember("b:2");
        s.remember("a:1");
        assert_eq!(s.recents, vec!["a:1", "b:2"]);
        assert_eq!(s.endpoint, "a:1");
    }

    #[test]
    fn the_recents_list_stays_short() {
        let mut s = Settings::default();
        for i in 0..20 {
            s.remember(&format!("host{i}:23554"));
        }
        assert_eq!(s.recents.len(), MAX_RECENTS);
        assert_eq!(s.recents[0], "host19:23554");
    }

    #[test]
    fn an_unreadable_file_yields_defaults_rather_than_an_error() {
        let missing = std::env::temp_dir().join("coyote-bridge-does-not-exist.json");
        let s = Settings::load(&missing);
        assert_eq!(s.http_port, 8787);
        assert!(s.recents.is_empty());
    }

    fn temp_path(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("coyote-bridge-{name}.json"));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("lock"));
        path
    }

    /// **The revocation race.** Two instances, one revokes, the other saves.
    ///
    /// This is the bug that made the revoke button a lie: instance B read the
    /// old token at startup, knows nothing about A's revocation, and used to
    /// write the whole struct back — reviving a credential the user had
    /// deliberately killed, with nothing to tell them.
    #[test]
    fn a_revoked_token_is_not_resurrected_by_another_instance() {
        let path = temp_path("revoke-race");

        // Both instances start from the same file.
        let mut first = Settings::default();
        let original = first.token();
        first.save(&path);
        let mut second = Settings::load(&path);
        assert_eq!(second.token.as_deref(), Some(original.as_str()));

        // Instance A revokes.
        let fresh = coyote_bridge::auth::Token::generate();
        first.set_token(&fresh);
        first.save(&path);

        // Instance B saves for an unrelated reason — a connect, say.
        second.remember("192.168.0.20:23554");
        second.save(&path);

        let on_disk = Settings::load(&path);
        assert_eq!(
            on_disk.token.as_deref(),
            Some(fresh.as_str()),
            "the revoked token came back from the dead"
        );
        assert_ne!(on_disk.token.as_deref(), Some(original.as_str()));
        // And B's own change still landed — deferring on the token must not
        // mean discarding everything else.
        assert_eq!(on_disk.endpoint, "192.168.0.20:23554");

        let _ = std::fs::remove_file(&path);
    }

    /// The instance that revoked keeps its own token, even if another
    /// instance wrote after it.
    #[test]
    fn the_instance_that_rotated_keeps_writing_its_own_token() {
        let path = temp_path("rotate-owner");

        let mut owner = Settings::default();
        let first_token = owner.token();
        owner.save(&path);

        let mut bystander = Settings::load(&path);
        bystander.remember("a:1");
        bystander.save(&path);

        let second_token = coyote_bridge::auth::Token::generate();
        owner.set_token(&second_token);
        owner.save(&path);

        assert_eq!(
            Settings::load(&path).token.as_deref(),
            Some(second_token.as_str())
        );
        assert_ne!(second_token.as_str(), first_token.as_str());
        let _ = std::fs::remove_file(&path);
    }

    /// A bystander adopts the on-disk token rather than carrying a stale one
    /// forward in memory, so a later save cannot revive it either.
    #[test]
    fn a_bystander_adopts_the_token_it_found() {
        let path = temp_path("adopt");

        let mut owner = Settings::default();
        owner.token();
        owner.save(&path);
        let mut bystander = Settings::load(&path);

        let fresh = coyote_bridge::auth::Token::generate();
        owner.set_token(&fresh);
        owner.save(&path);

        bystander.save(&path);
        assert_eq!(
            bystander.token.as_deref(),
            Some(fresh.as_str()),
            "the bystander should have adopted the newer token in memory too"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// A torn write must not leave a file that parses as defaults — that
    /// would mint a fresh token on next launch and un-pair every device.
    #[test]
    fn a_failed_write_leaves_the_previous_settings_intact() {
        let path = temp_path("atomic");

        let mut good = Settings::default();
        let token = good.token();
        good.remember("192.168.0.20:23554");
        good.save(&path);

        // Simulate the debris a killed process leaves behind.
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, "{ this is not json").unwrap();

        let loaded = Settings::load(&path);
        assert_eq!(loaded.token.as_deref(), Some(token.as_str()));
        assert_eq!(loaded.endpoint, "192.168.0.20:23554");

        let _ = std::fs::remove_file(&temp);
        let _ = std::fs::remove_file(&path);
    }

    /// Every field survives a save.
    ///
    /// The exhaustive literal in `merge_and_write` makes *forgetting* a new
    /// field a compile error. This catches the other half: wiring it in
    /// wrongly, so it round-trips as a default instead of what was set. Set
    /// everything to a non-default value, save, load, and require equality.
    ///
    /// If a new field makes this fail, the fix is in `merge_and_write` — not
    /// here.
    #[test]
    fn no_field_is_lost_across_a_save() {
        let path = temp_path("round-trip-all-fields");

        let mut original = Settings::default();
        original.remember("192.168.0.20:23554");
        original.remember("10.0.0.7:9999");
        original.http_port = 9001;
        original.static_dir = Some("C:/somewhere/dist".to_string());
        let token = original.token();
        original.save(&path);

        let mut loaded = Settings::load(&path);
        // Not persisted by design: it is a claim about a process, not a file.
        assert!(!loaded.token_is_ours);
        loaded.token_is_ours = original.token_is_ours;

        assert_eq!(
            loaded, original,
            "a field was dropped or defaulted on the way through save/load"
        );
        assert_eq!(loaded.token.as_deref(), Some(token.as_str()));
        assert_eq!(loaded.recents.len(), 2);

        let _ = std::fs::remove_file(&path);
    }

    /// The whole reason the token is persisted: a home-screen shortcut must
    /// keep working across restarts.
    #[test]
    fn the_token_survives_a_restart() {
        let path = std::env::temp_dir().join("coyote-bridge-token-test.json");
        let _ = std::fs::remove_file(&path);

        let mut first = Settings::default();
        let minted = first.token();
        first.save(&path);

        let mut second = Settings::load(&path);
        assert_eq!(
            second.token().as_str(),
            minted.as_str(),
            "a new token every launch would break the phone's saved URL"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_token_is_minted_once_and_then_reused() {
        let mut s = Settings::default();
        assert!(s.token.is_none());
        let first = s.token();
        let second = s.token();
        assert_eq!(first.as_str(), second.as_str());
        assert!(s.token.is_some(), "minting must persist into the settings");
    }

    #[test]
    fn settings_round_trip_through_disk() {
        let path = std::env::temp_dir().join("coyote-bridge-settings-test.json");
        let mut original = Settings::default();
        original.remember("192.168.1.50:23554");
        original.http_port = 9000;
        original.save(&path);

        let loaded = Settings::load(&path);
        assert_eq!(loaded.endpoint, "192.168.1.50:23554");
        assert_eq!(loaded.http_port, 9000);
        let _ = std::fs::remove_file(&path);
    }
}
