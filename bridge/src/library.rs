//! Serving a directory of funscripts to the phone.
//!
//! The bridge already runs on the machine where the media lives, already serves
//! the PWA, and already knows what the player is playing. Serving the scripts
//! too removes the only remaining manual step — importing files one at a time
//! into browser storage, which on iOS is genuinely miserable because there is
//! no directory picker.
//!
//! Two endpoints, both token-gated exactly like `/healthz`:
//!
//! ```text
//! GET /library/index.json   -> { "scripts": [ { name, bytes, modifiedMs } ], ... }
//! GET /library/<name>       -> the funscript bytes
//! ```
//!
//! plus a `library` WebSocket message that means **re-fetch the index**.
//!
//! ## What this module deliberately does not do
//!
//! **It does not match scripts to media.** The client already does, in
//! `src/lib/script/naming.ts` in the `coyote-socket-web` repo: the
//! MultiFunPlayer suffix convention (`.stroke`/`.up` -> L0, `.surge`/`.forward`
//! -> L1, `.sway`/`.left` -> L2, `.twist`/`.yaw` -> R0, `.roll` -> R1,
//! `.pitch` -> R2), a bare `<stem>.funscript` meaning stroke, both
//! `movie.funscript` and `movie.mp4.funscript` accepted, DLNA URLs handled, and
//! a documented tie-break. That is tested and merged. A second implementation
//! here would diverge from it, and the divergence would show up as "the script
//! I can see in the list does not load", which is a miserable thing to debug
//! across two languages.
//!
//! So this serves **names**, and the client matches. It receives `path` in
//! every snapshot, so it already knows what is playing.
//!
//! **It does not read the files.** Name, size and mtime come from the directory
//! entry. Parsing ten thousand funscripts to validate them would turn a
//! directory listing into a startup stall, and a file that will not parse is
//! the client's problem to report at load time — where it can say which script
//! and which axis — not ours to pre-empt at index time.
//!
//! ## The index is a snapshot, not a subscription
//!
//! Every response carries `scannedAtMs` and `ageMs`, so a consumer can see how
//! stale it is rather than assuming it is live. Contents are never pushed. The
//! WebSocket message carries only a generation counter and a count; it says
//! "this changed, ask again". Pushing contents would mean a second copy of the
//! listing with its own staleness, arriving over a channel that
//! [`crate::http::ws_relay`] documents as collapsing unboundedly under a slow
//! consumer.
//!
//! ## Scale
//!
//! A poller holds the answer so a request never touches the disk:
//!
//! - Every [`DIR_POLL`] it stats the root directory — one syscall — and
//!   rescans only if the directory's own mtime moved. Creating, deleting or
//!   renaming a file moves it; that is what "the directory contents changed"
//!   means.
//! - Every [`FULL_RESCAN`] it rescans regardless, because editing a file *in
//!   place* changes its size and mtime without touching the directory's. That
//!   only affects the two advisory fields — the bytes served by
//!   `GET /library/<name>` are always read from disk at request time.
//!
//! A rescan is one `read_dir` plus one `metadata` per entry. Measured, not
//! estimated — `scanning_ten_thousand_entries_stays_cheap` in this module's
//! tests does it and prints the numbers, so anyone can re-measure on their own
//! disk rather than trusting this paragraph:
//!
//! > **10,000 entries: 14 ms.** Windows 11, NVMe, release build. The
//! > filesystem cache was warm — the test had just written the files — so this
//! > is the steady-state figure, which is the one that matters for a poller.
//! > A genuinely cold first scan was not measured and will be slower.
//!
//! At the 60 s floor that is 0.02% of one core; the 2 s dir-stat poll is one
//! syscall and unmeasurable. The scan runs on `spawn_blocking`, so it never
//! occupies a runtime worker.
//!
//! The index for those 10,000 entries is **673 KB of JSON**, served in one
//! response on every client fetch. That is the number that would eventually
//! force pagination or an If-None-Match, not the scan — and `generation` is
//! already the etag-shaped thing to hang a conditional fetch on when it does.
//!
//! ## Scope
//!
//! **Flat, one level.** Subdirectories are not descended. Recursion is where
//! symlink loops, unbounded depth and a name that is no longer a single path
//! segment all arrive at once, and the client matches on a filename anyway.
//! Worth revisiting with a depth cap and a visited-inode set; not worth
//! smuggling in behind a directory listing.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::sync::watch;

use crate::logging::now_ms;
use crate::{log_info, log_warn};

/// How often the root directory's own mtime is checked. One `stat`.
pub const DIR_POLL: Duration = Duration::from_secs(2);

/// How often a full rescan happens regardless, to pick up in-place edits that
/// leave the directory's mtime untouched.
pub const FULL_RESCAN: Duration = Duration::from_secs(60);

/// Refuse to buffer a single "funscript" larger than this.
///
/// [`crate::http::respond`] writes a whole body from memory, so a 2 GB file
/// with the right extension dropped into the library would be a 2 GB
/// allocation. Real funscripts are tens of kilobytes; the largest multi-axis
/// packs seen are single-digit megabytes. 64 MB is far past anything genuine
/// and far short of anything that hurts.
pub const MAX_SCRIPT_BYTES: u64 = 64 * 1024 * 1024;

/// The extension a file must carry to be listed or served. Lowercased before
/// comparison — Windows and SMB shares do not agree with anyone about case.
const FUNSCRIPT_EXT: &str = "funscript";

/// One file, as the directory entry describes it. Never opened.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScriptEntry {
    /// The file name, exactly as it must be requested back.
    pub name: String,
    pub bytes: u64,
    /// Last-modified, epoch milliseconds. `0` when the filesystem will not say.
    pub modified_ms: u64,
}

/// A complete listing, as of one moment.
#[derive(Debug, Clone, Default)]
pub struct Index {
    /// Sorted by `name`, so lookup is a binary search and the order a client
    /// renders is stable across rescans.
    pub scripts: Vec<ScriptEntry>,
    /// When this scan finished, epoch milliseconds.
    pub scanned_at_ms: u64,
    /// Bumped whenever the contents differ from the previous scan. The
    /// `library` WebSocket message carries it so a client can tell a message it
    /// has already acted on from one it has not.
    pub generation: u64,
}

impl Index {
    fn position(&self, name: &str) -> Option<usize> {
        self.scripts
            .binary_search_by(|e| e.name.as_str().cmp(name))
            .ok()
    }
}

/// A directory of funscripts, kept indexed in the background.
pub struct Library {
    root: PathBuf,
    rx: watch::Receiver<Arc<Index>>,
}

impl Library {
    /// Start indexing `root` and return a handle.
    ///
    /// Returns immediately with an empty index; the first scan lands within a
    /// tick. A root that does not exist is not an error — a library is optional
    /// and a directory that appears later is picked up by the poller, which is
    /// the right behaviour for a network share that mounts after login.
    pub fn spawn(root: PathBuf) -> Arc<Self> {
        let (tx, rx) = watch::channel(Arc::new(Index::default()));
        let scan_root = root.clone();
        tokio::spawn(async move { poll(scan_root, tx).await });
        Arc::new(Self { root, rx })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The most recent listing. Never touches the disk.
    pub fn current(&self) -> Arc<Index> {
        self.rx.borrow().clone()
    }

    /// Wakes whenever the listing changes. Carries no contents by design.
    pub fn subscribe(&self) -> watch::Receiver<Arc<Index>> {
        self.rx.clone()
    }
}

// ---------------------------------------------------------------------------
// The index response
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct IndexResponse<'a> {
    scripts: &'a [ScriptEntry],
    /// Whether a library directory is configured at all.
    ///
    /// Distinguishes "you have not pointed me at a folder" from "the folder is
    /// empty", which want different things said in the UI. No library is a
    /// normal state, so both are a 200 with an empty list rather than an error.
    configured: bool,
    /// When the listing was taken, epoch milliseconds.
    scanned_at_ms: u64,
    /// How old it is, right now. The index is a snapshot; this is how a
    /// consumer sees that without doing clock arithmetic against a bridge whose
    /// clock it has no reason to trust.
    age_ms: u64,
    generation: u64,
}

/// Render `GET /library/index.json`.
pub fn index_json(library: Option<&Library>) -> Vec<u8> {
    let now = now_ms();
    let index = library.map(|l| l.current()).unwrap_or_default();
    let body = IndexResponse {
        scripts: &index.scripts,
        configured: library.is_some(),
        scanned_at_ms: index.scanned_at_ms,
        age_ms: now.saturating_sub(index.scanned_at_ms),
        generation: index.generation,
    };
    serde_json::to_vec(&body).unwrap_or_else(|_| b"{\"scripts\":[]}".to_vec())
}

/// The `library` WebSocket message: "re-fetch the index".
///
/// Deliberately carries no contents. See the module docs.
pub fn change_message(index: &Index) -> String {
    serde_json::json!({
        "type": "library",
        "generation": index.generation,
        "count": index.scripts.len(),
        "scannedAtMs": index.scanned_at_ms,
    })
    .to_string()
}

// ---------------------------------------------------------------------------
// Serving one file
// ---------------------------------------------------------------------------

/// What happened to a `GET /library/<name>`.
#[derive(Debug)]
pub enum Fetched {
    Ok(Vec<u8>),
    /// The name did not survive decoding or validation. 403 rather than 404 so
    /// a traversal attempt is distinguishable in a log from a typo.
    Rejected,
    NotFound,
    /// Bigger than [`MAX_SCRIPT_BYTES`].
    TooLarge(u64),
}

/// Read one script by the name the index published.
///
/// # What a hostile `name` can reach
///
/// Nothing outside the listing. Four independent gates, in this order:
///
/// 1. **Decode, then validate — never the reverse.** [`percent_decode`] runs
///    first, so `%2e%2e%2f` becomes `../` *before* anything inspects it, and
///    validating the encoded form (where `..` is invisible) is not a mistake
///    this function is able to make. A malformed escape is rejected outright
///    rather than passed through as a literal `%`.
/// 2. **No separators.** A decoded `/` or `\` is refused before path handling
///    sees it, which also settles the platform difference where `a\..\b` is
///    three components on Windows and one on Linux.
/// 3. **[`crate::http::safe_relative_path`]**, the same function `serve_static`
///    uses — one shared implementation, so a future fix lands in both places.
///    It accepts only `Component::Normal`, which rules out `..`, `.`, absolute
///    paths, Windows drive prefixes and UNC roots. The result must be exactly
///    one component.
/// 4. **It must be in the current index.** The strongest of the four, and the
///    reason the others are belt and braces: the only names that resolve are
///    ones this directory listing has already published. A name the scan never
///    produced cannot be fetched however it is spelled.
///
/// ## The one escape, and why it is left open
///
/// `safe_relative_path` does not `canonicalize`, which is a known reviewer
/// finding on `serve_static`. So a **symlink or junction inside the library
/// root**, named `something.funscript`, pointing at a file elsewhere, would be
/// listed by the scan and then served.
///
/// Left open deliberately. Placing that symlink requires write access to a
/// directory the user chose and pointed the bridge at — and anyone with write
/// access to it can simply put the file there instead, which no amount of
/// canonicalisation prevents. Meanwhile symlinked and junctioned collections
/// are how people actually assemble a media library on both Windows and Linux,
/// and refusing them would break a legitimate and common layout to close a hole
/// that requires already having won.
///
/// The cost of being wrong about that is bounded: only a file named
/// `*.funscript` in the root is reachable, only over a token-gated endpoint,
/// and only as far as a `read`.
pub async fn fetch(library: Option<&Library>, raw_name: &str) -> Fetched {
    let Some(library) = library else {
        return Fetched::NotFound;
    };

    let Some(decoded) = percent_decode(raw_name) else {
        return Fetched::Rejected;
    };
    if decoded.contains('/') || decoded.contains('\\') {
        return Fetched::Rejected;
    }
    let Some(rel) = crate::http::safe_relative_path(&decoded) else {
        return Fetched::Rejected;
    };
    if rel.components().count() != 1 {
        return Fetched::Rejected;
    }
    if !has_funscript_extension(&rel) {
        return Fetched::Rejected;
    }

    // Gate four: the name must be one we published.
    let index = library.current();
    if index.position(&decoded).is_none() {
        return Fetched::NotFound;
    }

    let path = library.root.join(&rel);
    let Ok(file) = tokio::fs::File::open(&path).await else {
        return Fetched::NotFound;
    };
    // Size from the open handle rather than the index: the index is up to
    // FULL_RESCAN old, and this decides how much memory to allocate.
    let size = match file.metadata().await {
        Ok(meta) if meta.is_file() => meta.len(),
        // A directory named `x.funscript` is not a script; the scan skips
        // those, so reaching here means it changed underneath us.
        _ => return Fetched::NotFound,
    };
    if size > MAX_SCRIPT_BYTES {
        return Fetched::TooLarge(size);
    }

    let mut bytes = Vec::with_capacity(size as usize);
    use tokio::io::AsyncReadExt;
    let mut file = file;
    match file.read_to_end(&mut bytes).await {
        Ok(_) => Fetched::Ok(bytes),
        Err(e) => {
            log_warn!("[library] could not read {}: {e}", path.display());
            Fetched::NotFound
        }
    }
}

/// Decode `%XX` escapes.
///
/// Needed here and not in `serve_static` because funscript libraries are full
/// of spaces, and a space in a URL path arrives as `%20`. `serve_static`
/// deliberately does not decode, which means a file with a space in its name is
/// unservable there — a real defect, but a separate one, and widening it would
/// mean auditing every static asset path at the same time.
///
/// Strict on purpose:
///
/// - A `%` not followed by two hex digits is a **rejection**, not a literal.
///   Passing it through is how a decoder ends up disagreeing with the client
///   that encoded the name, and disagreement about what a name means is exactly
///   the class of bug path validation exists to prevent.
/// - The result must be valid UTF-8. A byte sequence that is not is not a name
///   any scan of ours produced.
/// - Control characters are refused. A decoded NUL truncates a path in several
///   C APIs underneath `std`, and nothing legitimate carries one.
/// - `+` is left alone. It means a space in `application/x-www-form-urlencoded`
///   and a literal plus in a path, and files really are named `Scene+1`.
pub fn percent_decode(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hi = bytes.get(i + 1).and_then(|b| (*b as char).to_digit(16))?;
            let lo = bytes.get(i + 2).and_then(|b| (*b as char).to_digit(16))?;
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    let decoded = String::from_utf8(out).ok()?;
    if decoded.chars().any(|c| c.is_control()) {
        return None;
    }
    Some(decoded)
}

fn has_funscript_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(FUNSCRIPT_EXT))
}

// ---------------------------------------------------------------------------
// Scanning
// ---------------------------------------------------------------------------

async fn poll(root: PathBuf, tx: watch::Sender<Arc<Index>>) {
    let mut last_dir_mtime: Option<SystemTime> = None;
    let mut last_full = tokio::time::Instant::now() - FULL_RESCAN;
    let mut generation = 0u64;
    let mut announced_missing = false;

    loop {
        let dir_mtime = tokio::fs::metadata(&root)
            .await
            .ok()
            .filter(|m| m.is_dir())
            .and_then(|m| m.modified().ok());

        if dir_mtime.is_none() && !announced_missing {
            log_warn!(
                "[library] {} is not a directory; the index stays empty until it appears",
                root.display()
            );
            announced_missing = true;
        } else if dir_mtime.is_some() {
            announced_missing = false;
        }

        let due = last_full.elapsed() >= FULL_RESCAN;
        if dir_mtime != last_dir_mtime || due {
            last_dir_mtime = dir_mtime;
            last_full = tokio::time::Instant::now();

            let scan_root = root.clone();
            let scripts = tokio::task::spawn_blocking(move || scan(&scan_root))
                .await
                .unwrap_or_default();

            let changed = tx.borrow().scripts != scripts;
            if changed {
                generation += 1;
                log_info!(
                    "[library] {} script(s) in {} (generation {generation})",
                    scripts.len(),
                    root.display()
                );
            }
            // Republish either way so `scannedAtMs` stays honest — but only
            // `changed` bumps the generation, and only a bumped generation
            // should make a client re-fetch.
            let index = Arc::new(Index {
                scripts,
                scanned_at_ms: now_ms(),
                generation,
            });
            if changed {
                if tx.send(index).is_err() {
                    return; // nothing left listening
                }
            } else {
                // `send_replace` on an unchanged listing would still wake every
                // relay, and a `library` message that means "nothing happened"
                // trains clients to ignore the ones that mean something.
                tx.send_if_modified(|slot| {
                    *slot = index;
                    false
                });
            }
        }

        tokio::time::sleep(DIR_POLL).await;
    }
}

/// One `read_dir`, one `metadata` per entry, sorted. Blocking; called from
/// `spawn_blocking`.
///
/// Duplicate names cannot occur from one `read_dir`, but the dedup is kept
/// because `position` binary-searches and a sorted list with duplicates would
/// make lookup depend on which one it landed on.
fn scan(root: &Path) -> Vec<ScriptEntry> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !has_funscript_extension(&path) {
            continue;
        }
        // `metadata` follows symlinks, which is the intended behaviour — see
        // `fetch`'s note on why a symlink inside a directory the user chose is
        // treated as a file the user put there.
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            // A name that is not UTF-8 cannot be put in JSON or asked for back.
            continue;
        };
        if !seen.insert(name.to_string()) {
            continue;
        }
        out.push(ScriptEntry {
            name: name.to_string(),
            bytes: meta.len(),
            modified_ms: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("coyote-library-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    fn index_of(scripts: Vec<ScriptEntry>) -> Index {
        Index {
            scripts,
            scanned_at_ms: 1_000,
            generation: 1,
        }
    }

    /// A library with a fixed listing and no poller, so path handling can be
    /// tested without waiting on a scan. Dropping the sender is fine — a
    /// `watch::Receiver` keeps serving the last value after its sender is gone.
    fn library_over(root: PathBuf, scripts: Vec<ScriptEntry>) -> Library {
        let (_tx, rx) = watch::channel(Arc::new(index_of(scripts)));
        Library { root, rx }
    }

    #[test]
    fn a_scan_lists_only_funscripts_and_sorts_them() {
        let dir = temp_dir("scan");
        write(&dir, "b.funscript", "{}");
        write(&dir, "a.stroke.funscript", "{}");
        write(&dir, "notes.txt", "hi");
        write(&dir, "movie.mp4", "x");
        std::fs::create_dir(dir.join("nested.funscript")).unwrap();

        let scripts = scan(&dir);
        let names: Vec<_> = scripts.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["a.stroke.funscript", "b.funscript"]);
        assert_eq!(scripts[1].bytes, 2);
        assert!(scripts[1].modified_ms > 0);
    }

    /// Case is folded on the extension, because a `.FunScript` from a Windows
    /// pack is the same file to everyone except a case-sensitive comparison.
    #[test]
    fn the_extension_match_is_case_insensitive() {
        let dir = temp_dir("case");
        write(&dir, "Loud.FUNSCRIPT", "{}");
        assert_eq!(scan(&dir).len(), 1);
    }

    /// The scale claim in the module docs, re-measurable rather than asserted.
    ///
    /// Ignored because it writes 10,000 files, which takes far longer than the
    /// thing being measured. Run it with:
    ///
    /// ```text
    /// cargo test --release --lib library::scanning_ten_thousand -- --ignored --nocapture
    /// ```
    ///
    /// The bound is deliberately loose — it is there to catch someone turning
    /// the scan into a per-file `open`, not to police filesystem variance.
    #[test]
    #[ignore = "writes 10,000 files; run explicitly to re-measure the scale claim"]
    fn scanning_ten_thousand_entries_stays_cheap() {
        let dir = temp_dir("scale");
        for i in 0..10_000 {
            write(&dir, &format!("clip-{i:05}.funscript"), "{}");
        }

        // Cold-ish, then warm — both are quoted in the module docs.
        let cold = std::time::Instant::now();
        let scripts = scan(&dir);
        let cold = cold.elapsed();
        let warm = std::time::Instant::now();
        let again = scan(&dir);
        let warm = warm.elapsed();

        assert_eq!(scripts.len(), 10_000);
        assert_eq!(scripts, again);
        println!("10,000 entries: cold {cold:?}, warm {warm:?}");
        let bytes = serde_json::to_vec(&scripts).unwrap().len();
        println!("index JSON: {} KB", bytes / 1024);

        assert!(
            warm < Duration::from_millis(2_000),
            "a warm scan of 10,000 entries took {warm:?}; something is opening files"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_directory_scans_to_nothing_rather_than_failing() {
        assert!(scan(Path::new("C:/definitely/not/here/at/all")).is_empty());
    }

    #[test]
    fn spaces_and_unicode_survive_a_round_trip_through_percent_encoding() {
        assert_eq!(
            percent_decode("My%20Movie.funscript").as_deref(),
            Some("My Movie.funscript")
        );
        assert_eq!(
            percent_decode("Sc%C3%A8ne.funscript").as_deref(),
            Some("Scène.funscript")
        );
        // `+` is a literal plus in a path.
        assert_eq!(
            percent_decode("Scene+1.funscript").as_deref(),
            Some("Scene+1.funscript")
        );
    }

    #[test]
    fn a_malformed_escape_is_refused_rather_than_taken_literally() {
        for bad in ["100%.funscript", "%zz.funscript", "a%2.funscript", "%"] {
            assert_eq!(percent_decode(bad), None, "{bad} should not decode");
        }
    }

    #[test]
    fn control_characters_do_not_survive_decoding() {
        assert_eq!(percent_decode("a%00b.funscript"), None);
        assert_eq!(percent_decode("a%0Ab.funscript"), None);
    }

    /// **Decoding must not reintroduce traversal.** Decode happens first, so
    /// `..` is visible to validation rather than hidden behind escapes.
    #[tokio::test]
    async fn an_encoded_traversal_is_refused() {
        let dir = temp_dir("traversal");
        write(&dir, "ok.funscript", "{}");
        let library = library_over(
            dir,
            vec![ScriptEntry {
                name: "ok.funscript".into(),
                bytes: 2,
                modified_ms: 1,
            }],
        );

        for evil in [
            "%2e%2e%2fsecrets.funscript",
            "%2E%2E%5Csecrets.funscript",
            "..%2f..%2fetc%2fpasswd",
            "../ok.funscript",
            "..%5C..%5CWindows%5Cwin.ini",
            "%2fetc%2fpasswd",
            "C%3A%5CWindows%5Cwin.ini",
            "sub/ok.funscript",
        ] {
            assert!(
                matches!(fetch(Some(&library), evil).await, Fetched::Rejected),
                "{evil} should have been rejected"
            );
        }
    }

    /// A name that is well-formed but was never listed resolves to nothing,
    /// even when the file is sitting right there.
    #[tokio::test]
    async fn only_names_the_index_published_are_served() {
        let dir = temp_dir("membership");
        write(&dir, "listed.funscript", "{\"a\":1}");
        write(&dir, "unlisted.funscript", "{\"b\":2}");
        let library = library_over(
            dir,
            vec![ScriptEntry {
                name: "listed.funscript".into(),
                bytes: 7,
                modified_ms: 1,
            }],
        );

        assert!(matches!(
            fetch(Some(&library), "listed.funscript").await,
            Fetched::Ok(b) if b == b"{\"a\":1}"
        ));
        assert!(matches!(
            fetch(Some(&library), "unlisted.funscript").await,
            Fetched::NotFound
        ));
    }

    #[tokio::test]
    async fn a_name_with_a_space_is_servable() {
        let dir = temp_dir("space");
        write(&dir, "My Movie.funscript", "{}");
        let library = library_over(
            dir,
            vec![ScriptEntry {
                name: "My Movie.funscript".into(),
                bytes: 2,
                modified_ms: 1,
            }],
        );
        assert!(matches!(
            fetch(Some(&library), "My%20Movie.funscript").await,
            Fetched::Ok(_)
        ));
    }

    #[tokio::test]
    async fn no_library_configured_is_a_normal_state() {
        assert!(matches!(
            fetch(None, "a.funscript").await,
            Fetched::NotFound
        ));

        let body = index_json(None);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["configured"], false);
        assert_eq!(v["scripts"].as_array().unwrap().len(), 0);
    }

    /// The response has to say how fresh it is, because it is a snapshot and a
    /// consumer that treats it as live will render a file that was deleted
    /// FULL_RESCAN ago.
    #[test]
    fn the_index_reports_its_own_age() {
        let dir = temp_dir("age");
        let library = library_over(
            dir,
            vec![ScriptEntry {
                name: "a.funscript".into(),
                bytes: 1,
                modified_ms: 1,
            }],
        );
        let v: serde_json::Value = serde_json::from_slice(&index_json(Some(&library))).unwrap();
        assert_eq!(v["configured"], true);
        assert_eq!(v["scannedAtMs"], 1_000);
        assert_eq!(v["generation"], 1);
        assert!(
            v["ageMs"].as_u64().unwrap() > 0,
            "a snapshot from 1970 is not fresh"
        );
        assert_eq!(v["scripts"][0]["name"], "a.funscript");
        assert_eq!(v["scripts"][0]["bytes"], 1);
        assert_eq!(v["scripts"][0]["modifiedMs"], 1);
    }

    /// The WebSocket message says "ask again" and nothing else. If it ever
    /// carries contents, there are two listings with two staleness stories.
    #[test]
    fn the_change_message_carries_no_contents() {
        let index = index_of(vec![ScriptEntry {
            name: "a.funscript".into(),
            bytes: 1,
            modified_ms: 1,
        }]);
        let v: serde_json::Value = serde_json::from_str(&change_message(&index)).unwrap();
        assert_eq!(v["type"], "library");
        assert_eq!(v["count"], 1);
        assert_eq!(v["generation"], 1);
        assert!(
            v.get("scripts").is_none(),
            "the message must not carry the listing"
        );
    }

    /// The poller picks up a new file and bumps the generation exactly once
    /// for it. This is the whole point of the WebSocket message: a phone that
    /// is already connected should not need a reload.
    #[tokio::test]
    async fn a_new_file_bumps_the_generation_once() {
        let dir = temp_dir("watch");
        write(&dir, "first.funscript", "{}");

        let library = Library::spawn(dir.clone());
        let mut rx = library.subscribe();

        // First scan.
        tokio::time::timeout(Duration::from_secs(5), rx.changed())
            .await
            .expect("the first scan should land")
            .unwrap();
        assert_eq!(rx.borrow_and_update().scripts.len(), 1);
        let first_gen = rx.borrow().generation;

        write(&dir, "second.funscript", "{}");

        tokio::time::timeout(Duration::from_secs(10), rx.changed())
            .await
            .expect("a new file should wake a connected client")
            .unwrap();
        let index = rx.borrow_and_update().clone();
        assert_eq!(index.scripts.len(), 2);
        assert_eq!(index.generation, first_gen + 1);

        // And an unchanged directory produces no further wake, so a `library`
        // message always means something happened.
        assert!(
            tokio::time::timeout(Duration::from_secs(4), rx.changed())
                .await
                .is_err(),
            "an unchanged directory must not wake clients"
        );
    }
}
