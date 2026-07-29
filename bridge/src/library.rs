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
//! stale it is rather than assuming it is live — both `null` until a scan has
//! actually succeeded, because a snapshot that was never taken has no age.
//! Contents are never pushed. The WebSocket message carries a generation, a
//! count and the scan state; it says "this changed, ask again". Pushing
//! contents would mean a second copy of the listing with its own staleness,
//! arriving over a channel that [`crate::http::ws_relay`] documents as
//! collapsing unboundedly under a slow consumer.
//!
//! ## Three states, because two of them lie
//!
//! `configured` (is a path set) and [`ScanState`] (could it be read) are
//! separate fields, and neither is a boolean doing two jobs. This is
//! `FOLLOW-UPS.md` §0 applied before it bit rather than after: a scan that
//! fails **keeps the previous listing** and reports `scan: "failed"`, instead
//! of replacing it with an empty one. A network share dropping for a single
//! poll tick otherwise tells every connected phone the library is empty, with
//! a fresh timestamp asserting the answer is milliseconds old — and network
//! shares are an advertised use case, not an edge.
//!
//! ## Scale
//!
//! A poller holds the answer so a request never touches the disk:
//!
//! - Every [`DIR_POLL`] it stats the root directory and opens it for one entry
//!   ([`probe_listing`]) — both O(1) — and rescans only if the mtime moved, or
//!   if the directory would not open. Creating, deleting or renaming a file
//!   moves the mtime; that is what "the directory contents changed" means.
//!
//!   The probe is there because the mtime alone is a *proxy*: a directory that
//!   stats fine but refuses `read_dir` leaves the mtime untouched, and the
//!   whole [`ScanState`] mechanism below is unreachable if nothing decides to
//!   scan. See `FOLLOW-UPS.md` §0a — this was found the hard way.
//! - Every [`FULL_RESCAN`] it rescans regardless, because editing a file *in
//!   place* changes its size and mtime without touching the directory's. That
//!   only affects the two advisory fields — the bytes served by
//!   `GET /library/<name>` are always read from disk at request time.
//!
//! **[`FULL_RESCAN`] is not the latency a user experiences.** Dropping a file
//! into the directory moves its mtime, so a new script appears within
//! [`DIR_POLL`]. The 60 s figure only bounds how stale `bytes` and
//! `modifiedMs` can get for a file edited in place. The one setup where it
//! bites is an SMB share, where the client caches directory metadata and the
//! mtime change can arrive late — so on a network share, 60 s is the worst case
//! for *noticing a new file at all*.
//!
//! A rescan is one `read_dir`, and metadata taken from the directory entry
//! rather than by opening each file. Measured, not estimated —
//! `scanning_ten_thousand_entries_stays_cheap` in this module's tests does it
//! and prints the numbers, so anyone can re-measure on their own disk rather
//! than trusting this paragraph:
//!
//! > **10,000 entries: 18-21 ms** across runs on Windows 11, NVMe, release
//! > build, filesystem cache warm. Treat it as "tens of milliseconds", not as
//! > a constant; a cold cache or a network share will be slower.
//!
//! Even at the top of that range the 60 s floor is under 0.04% of one core, and
//! the 2 s dir-stat poll is one syscall. The scan runs on `spawn_blocking`, so
//! it never occupies a runtime worker.
//!
//! **Where the metadata comes from is the whole of that number.** Calling
//! `fs::metadata` on every entry — the obvious way to follow symlinks — opens
//! each file by path and measured **520-850 ms** for the same 10,000 entries,
//! 40x worse, because `read_dir` has already returned everything the common
//! case needs. `scan` asks `file_type()` first, which is free, and pays for a
//! real `stat` only on the entries that are actually links.
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

/// How the most recent scan attempt went.
///
/// **Three values, not two**, and deliberately separate from `configured`.
/// Whether a path is set and whether it could be read are different facts with
/// different things to say about them, and collapsing them is the shape
/// `FOLLOW-UPS.md` §0 exists to warn about: an optimistic default that makes
/// the UI confident about something nothing confirmed.
///
/// - `Pending` — configured, nothing scanned yet. True for the first couple of
///   seconds of every start. Distinguishable from `Ok` with an empty listing,
///   which it previously was not.
/// - `Ok` — the last scan read the directory. The listing is what was there.
/// - `Failed` — the last attempt could not read the directory. **The listing is
///   the last one that worked**, not an empty one, because a scan that failed
///   established nothing and must not be allowed to publish a fact.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ScanState {
    /// The default, and deliberately so: a fresh `Index` has established
    /// nothing, and `Ok` would be a claim nothing has confirmed.
    #[default]
    Pending,
    Ok,
    Failed,
}

/// A complete listing, as of one moment.
#[derive(Debug, Clone, Default)]
pub struct Index {
    /// Sorted by `name`, so lookup is a binary search and the order a client
    /// renders is stable across rescans.
    ///
    /// Always the most recent *successful* listing. A failed scan leaves this
    /// alone.
    pub scripts: Vec<ScriptEntry>,
    /// When the listing in `scripts` was taken, epoch milliseconds. `None`
    /// until a scan has succeeded — which is not the same as "taken at time
    /// zero", and reporting it as such made an unconfigured bridge describe its
    /// snapshot as fifty-six years old.
    pub scanned_at_ms: Option<u64>,
    /// When a scan was last *attempted*, successfully or not. Moves while
    /// `scanned_at_ms` stands still exactly when something is wrong.
    pub checked_at_ms: u64,
    /// How the last attempt went. See [`ScanState`].
    pub state: ScanState,
    /// Bumped whenever the contents differ from the previous *successful*
    /// scan. The `library` WebSocket message carries it so a client can tell a
    /// message it has already acted on from one it has not.
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
///
/// # Names are matched exactly, and that is a contract
///
/// `GET /library/<name>` compares the decoded name to the index **byte for
/// byte**: no case folding, no Unicode normalisation. It works because a client
/// asks for a name it read out of the index verbatim, and it is the only
/// comparison that cannot disagree with the filesystem — Windows and SMB fold
/// case, Linux does not, and macOS normalises to NFD while Windows preserves
/// whatever was typed.
///
/// **The client must round-trip names unchanged.** `naming.ts` deliberately
/// case-folds for *matching a script to media*, which is the right thing there.
/// If that folded name were ever used as the *fetch key*, every pack whose
/// files disagree with its videos about capitalisation would 404 — and it would
/// look like a missing file rather than a lookup bug, which is the expensive
/// kind of wrong. Match with the folded name; fetch with the listed one.
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
    /// Whether the last scan attempt worked, and whether there has been one.
    /// Orthogonal to `configured`: a path can be set and unreadable.
    scan: ScanState,
    /// When the listing was taken, epoch milliseconds. **`null` until a scan
    /// has succeeded**, rather than `0` — an index that has never been taken
    /// has no age, and saying it was taken in 1970 is a worse answer than
    /// saying nothing.
    scanned_at_ms: Option<u64>,
    /// How old the listing is, right now, or `null` if there is not one yet.
    /// The index is a snapshot; this is how a consumer sees that without doing
    /// clock arithmetic against a bridge whose clock it has no reason to trust.
    ///
    /// Read it together with `scan`: when that is `failed`, this is the age of
    /// the last listing that worked and is the number that says how far behind
    /// reality it might be.
    age_ms: Option<u64>,
    /// When a scan was last attempted, successfully or not.
    checked_at_ms: u64,
    generation: u64,
}

/// Render `GET /library/index.json`.
pub fn index_json(library: Option<&Library>) -> Vec<u8> {
    let now = now_ms();
    let index = library.map(|l| l.current()).unwrap_or_default();
    let body = IndexResponse {
        scripts: &index.scripts,
        configured: library.is_some(),
        scan: index.state,
        scanned_at_ms: index.scanned_at_ms,
        age_ms: index.scanned_at_ms.map(|at| now.saturating_sub(at)),
        checked_at_ms: index.checked_at_ms,
        generation: index.generation,
    };
    serde_json::to_vec(&body).unwrap_or_else(|_| b"{\"scripts\":[]}".to_vec())
}

/// The `library` WebSocket message: "re-fetch the index".
///
/// Deliberately carries no contents. See the module docs.
///
/// `scan` rides along because a client that only listens to the socket would
/// otherwise see a `failed` scan as silence, and silence is the one thing the
/// relay's contract promises means something else entirely.
pub fn change_message(index: &Index) -> String {
    serde_json::json!({
        "type": "library",
        "generation": index.generation,
        "count": index.scripts.len(),
        "scan": index.state,
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
/// Nothing outside the listing. Four checks, in this order — but they are
/// **not four independent gates, and describing them that way overstates the
/// redundancy.** Three are cheap syntactic filters; the fourth is the one that
/// holds:
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
///    one component with a `.funscript` extension.
/// 4. **It must be in the current index.** Not a belt to the others' braces —
///    **the load-bearing one.** NTFS alternate data streams (`a.funscript:x`),
///    Windows device names (`CON`, `NUL`), 8.3 short-name aliases, double
///    percent-encoding, trailing dots and spaces, and overlong UTF-8 all clear
///    checks 1-3 unchanged: none of them contains a separator or a `..`
///    component. They die here, because none of them is a name a `read_dir`
///    ever produced.
///
/// **So check 4 must never be relaxed**, and the rule that keeps it intact is
/// one line rather than a list:
///
/// > ***Any lookup that is not byte-exact* relaxes check 4.**
///
/// Case folding is one instance of that, not the definition of it, and reading
/// the guard as "do not case-fold" is how the next hole gets opened by someone
/// who was being careful. Others, none of which are case folding:
///
/// - **Unicode normalisation-insensitive lookup.** NFC vs NFD — the most likely
///   one to be proposed, the first time a Mac-authored pack lands in a library
///   served from Windows.
/// - **Trailing dot or space leniency.** Win32 strips them, so `a.funscript `
///   and `a.funscript.` open the same file while comparing unequal.
/// - **Subdirectories**, a **freshly dropped file before the next poll**, or
///   anything else that serves a name the scan did not publish.
///
/// Each of those has to be handled explicitly *before* check 4 is loosened,
/// because the syntactic checks above stop none of them.
///
/// ## Symlinks: followed, deliberately, on both paths
///
/// `fs::metadata` in the scan and `File::open` here both follow links, so a
/// symlink or junction named `*.funscript` inside the library root is listed
/// and served like any other file. That is the policy, and the two halves now
/// agree — they did not before: the scan used `DirEntry::metadata`, which has
/// `lstat` semantics, so symlinks were silently *excluded* from the listing
/// while this comment claimed they were included. A library assembled as a
/// folder of links into three drives indexed as empty, with nothing logged.
///
/// Following is the right policy because that layout is how media libraries are
/// actually assembled, and because refusing it would close nothing: placing a
/// link requires write access to a directory the user chose, and anyone with
/// that access can put the file there directly.
///
/// `safe_relative_path` still does not `canonicalize` — the known finding on
/// `serve_static` — so this is a real reach outside the root, accepted with its
/// eyes open. It is bounded to a `*.funscript` in the root, over a token-gated
/// endpoint, read-only.
///
/// One residue, and a small one now that following is the policy on both
/// paths: between the scan listing a regular file and this opening it, the file
/// can be replaced by a symlink. The fetch follows it — but so would the next
/// scan, and the listing would then say so. **Nothing is reachable through that
/// window that a rescan would not publish anyway**; what actually goes stale is
/// the advisory `bytes` and `modifiedMs`, for at most [`DIR_POLL`].
///
/// Worth saying precisely rather than dramatically: an earlier draft described
/// this as the fetch following a link into somewhere it should not go, which
/// implied a reach the policy already grants openly. That is the same
/// comment-versus-behaviour drift that produced this module's first blocker,
/// and it is worth not repeating in the paragraph explaining it.
pub async fn fetch(library: Option<&Library>, raw_name: &str) -> Fetched {
    let Some(library) = library else {
        return Fetched::NotFound;
    };

    // `/library/` with nothing after it. Refused explicitly, because
    // `safe_relative_path("")` answers `index.html` — a sensible default for a
    // static server and a trapdoor here. Only the extension check currently
    // stops the bridge handing back the PWA shell as a funscript, and relying
    // on that is relying on a coincidence between two unrelated rules.
    if raw_name.is_empty() {
        return Fetched::Rejected;
    }

    let Some(decoded) = percent_decode(raw_name) else {
        return Fetched::Rejected;
    };
    if decoded.is_empty() {
        return Fetched::Rejected;
    }
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

    // Check four, the load-bearing one: the name must be one we published.
    // Exact match, byte for byte — see `Library`'s note on case and Unicode
    // normalisation, which is a contract with the client, not an accident.
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

    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::with_capacity(size as usize);
    // `take`, not a bare `read_to_end`. The size check above is a `stat` and
    // the read that follows it is a separate syscall; a file being appended to
    // between the two would be buffered whole, which is the allocation the cap
    // exists to prevent. One extra byte, so "grew past the cap" is detectable
    // rather than silently truncating a script into invalid JSON.
    let mut file = file.take(MAX_SCRIPT_BYTES + 1);
    match file.read_to_end(&mut bytes).await {
        Ok(_) if bytes.len() as u64 > MAX_SCRIPT_BYTES => Fetched::TooLarge(bytes.len() as u64),
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
    // Tracked separately from the listing, because the two do not change
    // together. The warning used to sit inside `if changed`, so a library whose
    // *only* file was an oversized pack produced an empty listing that was not
    // a change from the default empty `Index` — no listing change, therefore no
    // warning. The user then sees `scan: "ok"`, zero scripts and nothing in the
    // log: the "told nothing at all" outcome that excluding oversized files was
    // meant to avoid, in precisely the case where they have no other clue.
    // With any second normal file present it fired, which is what made it easy
    // to miss.
    let mut last_oversize: Vec<(String, u64)> = Vec::new();

    loop {
        let dir_mtime = tokio::fs::metadata(&root)
            .await
            .ok()
            .filter(|m| m.is_dir())
            .and_then(|m| m.modified().ok());

        // Ask, every tick, the same question the scan depends on: **can this
        // directory be read?** O(1) — open it and take one entry.
        //
        // This gate decides whether the check happens, and it used to be a
        // stat-level proxy for a read-level failure: `dir_mtime.is_none()`. A
        // directory whose mtime has not moved and which still stats fine, but
        // whose `read_dir` is refused, matched no clause. No scan ran, so no
        // verdict was produced, so `state` stayed `Ok` and `checkedAtMs` froze
        // for up to FULL_RESCAN. Every bit of the failure machinery below was
        // unreachable, because reaching it required a scan nobody attempted.
        //
        // Note what does *not* fix it: `state != ScanState::Ok`. That retries
        // once a failure is known and covers `Pending`, but the transition into
        // failure is exactly when `state` is still `Ok`, so the first refusal is
        // still missed. Asserted by
        // `a_directory_that_stats_but_will_not_list_is_reported_as_failed`,
        // which was written against that version and failed.
        //
        // A probe is the honest gate because it is the same operation, not a
        // proxy for it. It subsumes `is_none()` too: a root that will not stat
        // will not open either.
        let probe_root = root.clone();
        let readable = tokio::task::spawn_blocking(move || probe_listing(&probe_root))
            .await
            .unwrap_or_else(|e| Err(std::io::Error::other(format!("probe task failed: {e}"))));

        let due = last_full.elapsed() >= FULL_RESCAN;
        if readable.is_err()
            || dir_mtime != last_dir_mtime
            || due
            || tx.borrow().state != ScanState::Ok
        {
            last_dir_mtime = dir_mtime;
            last_full = tokio::time::Instant::now();

            let scan_root = root.clone();
            // A `JoinError` — a panicked or cancelled scan — is a failure, not
            // an empty directory. `unwrap_or_default()` used to make those
            // indistinguishable, which is the same defect as `read_dir`
            // failing, arriving by a different route.
            let outcome = match tokio::task::spawn_blocking(move || scan(&scan_root)).await {
                Ok(Ok(scanned)) => Ok(scanned),
                Ok(Err(e)) => Err(e.to_string()),
                Err(e) => Err(format!("scan task failed: {e}")),
            };

            let previous = tx.borrow().clone();
            let now = now_ms();

            let next = match outcome {
                Ok(scanned) => {
                    let changed = previous.scripts != scanned.scripts;
                    if changed {
                        generation += 1;
                        log_info!(
                            "[library] {} script(s) in {} (generation {generation})",
                            scanned.scripts.len(),
                            root.display()
                        );
                    }
                    // On its own transition, not the listing's.
                    if scanned.oversize != last_oversize {
                        for (path, bytes) in &scanned.oversize {
                            log_warn!(
                                "[library] {path} is {bytes} bytes, over the {MAX_SCRIPT_BYTES} \
                                 byte limit — left out of the listing rather than served"
                            );
                        }
                        last_oversize = scanned.oversize.clone();
                    }
                    if previous.state == ScanState::Failed {
                        log_info!("[library] {} is readable again", root.display());
                    }
                    Index {
                        scripts: scanned.scripts,
                        scanned_at_ms: Some(now),
                        checked_at_ms: now,
                        state: ScanState::Ok,
                        generation,
                    }
                }
                Err(detail) => {
                    if previous.state != ScanState::Failed {
                        log_warn!(
                            "[library] could not read {}: {detail}. Keeping the last listing \
                             ({} script(s)) rather than reporting the library empty.",
                            root.display(),
                            previous.scripts.len()
                        );
                    }
                    // Everything about the listing is carried forward
                    // untouched. A scan that failed established nothing, so it
                    // gets to change nothing except the two fields that
                    // describe the attempt itself.
                    Index {
                        scripts: previous.scripts.clone(),
                        scanned_at_ms: previous.scanned_at_ms,
                        checked_at_ms: now,
                        state: ScanState::Failed,
                        generation,
                    }
                }
            };

            // Wake clients when the listing changed *or* when the scan state
            // did — a library that just became unreadable is news, and a client
            // that only watches the socket would otherwise see silence, which
            // the relay's contract promises means something else entirely.
            //
            // Not on `checkedAtMs` alone: republishing every tick would make a
            // `library` message mean "nothing happened", which trains clients
            // to ignore the ones that mean something.
            let worth_saying =
                next.generation != previous.generation || next.state != previous.state;
            let index = Arc::new(next);
            if worth_saying {
                if tx.send(index).is_err() {
                    return; // nothing left listening
                }
            } else {
                tx.send_if_modified(|slot| {
                    *slot = index;
                    false
                });
            }
        }

        tokio::time::sleep(DIR_POLL).await;
    }
}

/// What one scan found.
#[derive(Debug, Default)]
struct Scanned {
    scripts: Vec<ScriptEntry>,
    /// Files skipped for exceeding [`MAX_SCRIPT_BYTES`], as `(path, bytes)`.
    /// Reported so an excluded file is explained rather than merely absent.
    oversize: Vec<(String, u64)>,
}

/// One `read_dir`, one `metadata` per entry, sorted. Blocking; called from
/// `spawn_blocking`.
///
/// **`Err` means the directory could not be read, and is not the same as an
/// empty directory.** It used to return `Vec::new()` for both, which published
/// "your library is now empty" to every connected phone the first time a
/// network share hiccuped — with `configured: true` and a fresh `scannedAtMs`
/// asserting the answer was milliseconds old. That is `FOLLOW-UPS.md` §0
/// exactly: one value meaning both "no problem" and "no answer", with the
/// optimistic reading as the default.
///
/// Duplicate names cannot occur from one `read_dir`, but the dedup is kept
/// because `position` binary-searches and a sorted list with duplicates would
/// make lookup depend on which one it landed on.
fn scan(root: &Path) -> std::io::Result<Scanned> {
    let entries = std::fs::read_dir(root)?;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut oversize = Vec::new();
    // Counted, not `.flatten()`ed away.
    //
    // `read_dir` returning `Ok` only says the directory *opened*. Iteration is
    // where an SMB share actually fails, and that is the likelier of the two:
    // the handle is fine, 300 of 900 entries come back, and then the link
    // drops. `.flatten()` discarded those errors, so a truncated listing was
    // returned as `Ok` — bumping the generation, pushing a `library` message
    // and stamping a fresh `scannedAtMs` on an answer that was missing two
    // thirds of the library. Exactly the defect the rest of this module now
    // prevents, surviving in the one place it did not reach.
    let mut unreadable = 0usize;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                unreadable += 1;
                continue;
            }
        };
        let path = entry.path();
        if !has_funscript_extension(&path) {
            continue;
        }
        // Symlinks are followed — see `fetch` for why that is the policy — but
        // only symlinks pay for it.
        //
        // `DirEntry::metadata` has `lstat` semantics on every platform: for a
        // symlink it describes the link, so `is_file()` is false and the entry
        // would be dropped from the listing. That is what this code used to do
        // while its documentation claimed the opposite, so a library assembled
        // out of links into three drives indexed as empty with nothing logged.
        //
        // The obvious fix — `fs::metadata` for every entry — is correct and
        // **40x slower**: measured, a 10,000-entry scan went from 14 ms to
        // 520-850 ms, because on Windows `DirEntry` carries the metadata that
        // `read_dir` already returned while `fs::metadata` opens each file by
        // path. `file_type()` comes from the same cached data, so asking it
        // first is free and the extra `stat` is paid only by the entries that
        // actually need following.
        let Ok(link_type) = entry.file_type() else {
            continue;
        };
        let meta = if link_type.is_symlink() {
            // A broken link lands in the `Err` arm. Skipping it silently is
            // right: it names no file, so there is nothing to serve and
            // nothing the user can do about it from the phone.
            match std::fs::metadata(&path) {
                Ok(meta) => meta,
                Err(_) => continue,
            }
        } else {
            match entry.metadata() {
                Ok(meta) => meta,
                Err(_) => continue,
            }
        };
        if !meta.is_file() {
            continue;
        }
        // Oversized files are excluded from the listing rather than listed and
        // then refused. A client that renders a name it can only ever get a 413
        // for has been told about a script it cannot play, which is worse than
        // not being told. The names are returned so the poller can say so in
        // the log, where the user can act on it.
        if meta.len() > MAX_SCRIPT_BYTES {
            oversize.push((path.display().to_string(), meta.len()));
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
    // Sorted for the same reason `scripts` is: `poll` compares this against the
    // previous scan to decide whether to log, so an unsorted list would make
    // that comparison order-sensitive and a filesystem returning entries in a
    // different order would re-log the same file forever.
    oversize.sort();
    verdict(
        unreadable,
        Scanned {
            scripts: out,
            oversize,
        },
    )
}

/// Can this directory be listed right now? Open it and take one entry.
///
/// The cheap half of [`scan`], run every [`DIR_POLL`] so the decision to skip a
/// full scan rests on the operation that actually fails rather than on a `stat`
/// standing in for it. Catches both a refused open and an immediate iteration
/// error; costs one directory handle and one entry regardless of library size,
/// so it does not disturb the scale story — the full scan still runs only on a
/// change, on [`FULL_RESCAN`], or on a failure.
///
/// What it does **not** catch is an enumeration that fails partway through, at
/// entry 300 of 900, while the directory's mtime is untouched: that waits for
/// the next full scan. Narrower than the hole it closes, and stated rather than
/// discovered.
fn probe_listing(root: &Path) -> std::io::Result<()> {
    std::fs::read_dir(root)?.next().transpose()?;
    Ok(())
}

/// A partial enumeration is a failed scan, not a short library.
///
/// Split out from [`scan`] so the decision is testable. A mid-iteration
/// `read_dir` failure is an SMB behaviour and cannot be induced on a local
/// disk, so the loop that counts is exercised only by real directories — but
/// what to *do* with a non-zero count is the part that was wrong, and that is
/// checkable here.
///
/// Failing is the only honest answer: with entries missing we do not know what
/// is in the directory, and "fewer scripts than last time" is a claim, not the
/// absence of one. The caller keeps the previous listing.
fn verdict(unreadable: usize, scanned: Scanned) -> std::io::Result<Scanned> {
    if unreadable > 0 {
        return Err(std::io::Error::other(format!(
            "{unreadable} of the directory's entries could not be read; \
             treating the scan as failed rather than publishing a short listing"
        )));
    }
    Ok(scanned)
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
            scanned_at_ms: Some(1_000),
            checked_at_ms: 1_000,
            state: ScanState::Ok,
            generation: 1,
        }
    }

    fn scan_ok(root: &Path) -> Vec<ScriptEntry> {
        scan(root)
            .expect("the directory should be readable")
            .scripts
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

        let scripts = scan_ok(&dir);
        let names: Vec<_> = scripts.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["a.stroke.funscript", "b.funscript"]);
        assert_eq!(scripts[1].bytes, 2);
        assert!(scripts[1].modified_ms > 0);
    }

    /// Case is folded on the extension, because a `.FunScript` from a Windows
    /// pack is the same file to everyone except a case-sensitive comparison.
    /// Create a symlink, or fail the test saying why.
    ///
    /// **Deliberately not a skip.** This used to return `false` and let the
    /// caller `eprintln!` and return, which reports `ok` — so on a Windows CI
    /// box without Developer Mode the regression test for this PR's headline
    /// bug passed having asserted nothing, and libtest swallows `eprintln!`
    /// without `--nocapture`, so the skip was invisible in the log too.
    ///
    /// A test that silently verifies nothing is the exact failure this module
    /// spent a round fixing. Embedding one in the test that guards it would be
    /// absurd. If this panics, the machine cannot exercise symlink handling and
    /// that is a fact worth stopping for, not one to paper over: on Windows,
    /// enable Developer Mode (Settings → System → For developers).
    fn symlink_or_fail(target: &Path, link: &Path) {
        #[cfg(windows)]
        let result = std::os::windows::fs::symlink_file(target, link);
        #[cfg(unix)]
        let result = std::os::unix::fs::symlink(target, link);

        if let Err(e) = result {
            panic!(
                "could not create a symlink at {}: {e}\n\
                 This test cannot verify symlink handling on this machine. On \
                 Windows, enable Developer Mode (Settings > System > For \
                 developers). Failing rather than skipping, because a skip here \
                 reports `ok` and would silently stop guarding the bug this \
                 test exists for.",
                link.display()
            )
        }
    }

    /// **Symlinks are followed, and this is the test that says so.**
    ///
    /// The scan used `DirEntry::metadata`, which is `lstat` on every platform,
    /// so a symlinked script was silently missing from the listing — while the
    /// documentation said symlinked collections were the supported layout. The
    /// docs described `fs::metadata`; the code did the opposite. This asserts
    /// the behaviour the docs claim, so they cannot drift apart again silently.
    #[test]
    fn a_symlinked_script_is_indexed_and_reachable() {
        let dir = temp_dir("symlink");
        let outside = temp_dir("symlink-target");
        let target = outside.join("real.funscript");
        std::fs::write(&target, r#"{"actions":[]}"#).unwrap();

        let link = dir.join("linked.funscript");
        symlink_or_fail(&target, &link);

        let scripts = scan_ok(&dir);
        assert_eq!(
            scripts.len(),
            1,
            "a symlinked script must be listed; DirEntry::metadata would drop it"
        );
        assert_eq!(scripts[0].name, "linked.funscript");
        assert_eq!(
            scripts[0].bytes, 14,
            "the size must be the target's, not the link's"
        );
    }

    /// A link that names nothing is skipped rather than listed with junk
    /// metadata.
    #[test]
    fn a_broken_symlink_is_skipped() {
        let dir = temp_dir("broken-symlink");
        let link = dir.join("dangling.funscript");
        symlink_or_fail(Path::new("C:/nowhere/at/all.funscript"), &link);
        assert!(scan_ok(&dir).is_empty());
    }

    /// A file over the cap is left out of the listing entirely, rather than
    /// advertised and then refused with a 413 the client can do nothing about.
    #[test]
    fn an_oversized_file_is_not_listed() {
        let dir = temp_dir("oversize");
        std::fs::write(dir.join("fine.funscript"), "{}").unwrap();
        // `set_len` on a fresh file is instant on NTFS and ext4 — no 64 MB is
        // actually written.
        let big = std::fs::File::create(dir.join("huge.funscript")).unwrap();
        big.set_len(MAX_SCRIPT_BYTES + 1).unwrap();
        drop(big);

        let scanned = scan(&dir).unwrap();
        let names: Vec<_> = scanned.scripts.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["fine.funscript"]);
        assert_eq!(
            scanned.oversize.len(),
            1,
            "the exclusion must be reportable"
        );
        assert!(scanned.oversize[0].0.contains("huge.funscript"));
    }

    /// `scan` reports an oversized file even when the listing it produces is
    /// empty. **This is the precondition, not the fix** — see the test below,
    /// which is the one that would fail if the fix were reverted.
    #[test]
    fn a_scan_reports_an_oversized_file_even_with_an_empty_listing() {
        let dir = temp_dir("oversize-only");
        let big = std::fs::File::create(dir.join("huge.funscript")).unwrap();
        big.set_len(MAX_SCRIPT_BYTES + 1).unwrap();
        drop(big);

        let scanned = scan(&dir).unwrap();
        assert!(scanned.scripts.is_empty(), "nothing is servable here");
        assert_eq!(
            scanned.oversize.len(),
            1,
            "an empty listing must still carry the reason it is empty"
        );
    }

    /// **The fix itself: `poll` warns about an oversized file that is the only
    /// entry in the directory.**
    ///
    /// The warning used to be nested inside "the listing changed", and an empty
    /// listing is not a change from the default empty `Index` — so the one case
    /// where the user has no other clue was the one case that said nothing.
    ///
    /// Asserting on `scan`'s return value alone does not cover this: the
    /// precondition test above passes with the log statement moved back inside
    /// `if changed`. The only observable that distinguishes them is the log
    /// line, so this subscribes to the log tap and waits for it. Same family as
    /// the status-phrase table — an assertion that looks entirely reasonable
    /// and verifies the wrong side of the fix.
    #[tokio::test]
    async fn poll_warns_when_the_only_file_is_oversized() {
        let dir = temp_dir("oversize-only-poll");
        let big = std::fs::File::create(dir.join("huge.funscript")).unwrap();
        big.set_len(MAX_SCRIPT_BYTES + 1).unwrap();
        drop(big);

        // Subscribe before spawning, or the first scan's line is missed.
        let mut logs = crate::logging::subscribe();
        let library = Library::spawn(dir.clone());

        let found = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match logs.recv().await {
                    Ok(line) if line.contains("huge.funscript") && line.contains("over the") => {
                        return line
                    }
                    Ok(_) => continue,
                    Err(_) => panic!("the log tap closed"),
                }
            }
        })
        .await
        .expect(
            "a library whose only file is oversized must say so; \
             an empty listing with no explanation is the outcome excluding \
             oversized files was meant to avoid",
        );
        assert!(found.contains("[WARN]"), "got: {found}");

        // And the listing really is empty, so the log line is the user's only
        // signal — which is why it has to fire.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while library.current().state != ScanState::Ok {
            assert!(tokio::time::Instant::now() < deadline, "no scan landed");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(library.current().scripts.is_empty());
    }

    /// **A partial enumeration must not publish as a listing.**
    ///
    /// `read_dir` returning `Ok` says only that the directory opened. Iteration
    /// is where an SMB share actually drops — handle fine, 300 of 900 entries
    /// back, then the link goes — and `.flatten()` discarded exactly those
    /// errors, so a two-thirds-truncated listing went out as `scan: "ok"` with
    /// a bumped generation and a fresh timestamp.
    ///
    /// The counting loop needs a real network failure to exercise. The decision
    /// it feeds does not, and the decision is what was wrong.
    #[test]
    fn a_partial_enumeration_fails_rather_than_publishing_a_short_listing() {
        let found = Scanned {
            scripts: vec![ScriptEntry {
                name: "a.funscript".into(),
                bytes: 1,
                modified_ms: 1,
            }],
            oversize: Vec::new(),
        };

        assert!(
            verdict(0, found).is_ok(),
            "a clean enumeration is a listing"
        );

        let partial = Scanned {
            scripts: vec![ScriptEntry {
                name: "a.funscript".into(),
                bytes: 1,
                modified_ms: 1,
            }],
            oversize: Vec::new(),
        };
        let err = verdict(3, partial).expect_err("entries went missing; that is not a listing");
        assert!(
            err.to_string().contains("could not be read"),
            "the reason must survive into the log: {err}"
        );
    }

    #[test]
    fn the_extension_match_is_case_insensitive() {
        let dir = temp_dir("case");
        write(&dir, "Loud.FUNSCRIPT", "{}");
        assert_eq!(scan_ok(&dir).len(), 1);
    }

    /// The scale claim in the module docs, re-measurable rather than asserted.
    ///
    /// Ignored because it writes 10,000 files, which takes far longer than the
    /// thing being measured. Run it with:
    ///
    /// ```text
    /// cargo test --release --lib library::tests::scanning_ten_thousand -- --ignored --nocapture
    /// ```
    ///
    /// Note `tests::` — the module path, not just the module. Without it the
    /// filter matches nothing, and `cargo test` reports success for having run
    /// zero tests, which is how the number in the module docs came to be quoted
    /// from a single unrepresentative run.
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
        let scripts = scan_ok(&dir);
        let cold = cold.elapsed();
        let warm = std::time::Instant::now();
        let again = scan_ok(&dir);
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
    /// **A directory that cannot be read is an error, not an empty listing.**
    ///
    /// The two used to be the same value, which is how a network share
    /// hiccuping for one poll tick told every connected phone the library was
    /// empty.
    fn an_unreadable_directory_is_an_error_not_an_empty_listing() {
        assert!(scan(Path::new("C:/definitely/not/here/at/all")).is_err());

        let empty = temp_dir("genuinely-empty");
        assert_eq!(scan(&empty).unwrap().scripts.len(), 0);
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
            // `/library/` itself. `safe_relative_path("")` answers
            // `index.html`, so without an explicit refusal this leans on the
            // extension check to stop the PWA shell being served as a script.
            "",
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

    /// **The blocker: a share that drops for one tick must not tell every
    /// phone the library is empty.**
    ///
    /// Measured against the previous implementation: deleting the root took the
    /// listing from 1 script to 0 with `configured: true`, a bumped generation,
    /// and a fresh `scannedAtMs` asserting the answer was milliseconds old.
    /// Network shares are an advertised use case, so this is not a rare path.
    #[tokio::test]
    async fn a_vanished_root_keeps_the_last_listing_and_says_it_failed() {
        let dir = temp_dir("vanish");
        write(&dir, "only.funscript", "{}");

        let library = Library::spawn(dir.clone());
        let mut rx = library.subscribe();

        tokio::time::timeout(Duration::from_secs(5), rx.changed())
            .await
            .expect("the first scan should land")
            .unwrap();
        let good = rx.borrow_and_update().clone();
        assert_eq!(good.state, ScanState::Ok);
        assert_eq!(good.scripts.len(), 1);
        let good_gen = good.generation;
        let good_scanned = good.scanned_at_ms.expect("a successful scan has a time");

        std::fs::remove_dir_all(&dir).unwrap();

        tokio::time::timeout(Duration::from_secs(10), rx.changed())
            .await
            .expect("becoming unreadable is news and must wake clients")
            .unwrap();
        let bad = rx.borrow_and_update().clone();

        assert_eq!(bad.state, ScanState::Failed);
        assert_eq!(
            bad.scripts.len(),
            1,
            "a scan that failed established nothing and must not empty the listing"
        );
        assert_eq!(
            bad.generation, good_gen,
            "a failed scan is not a content change"
        );
        assert_eq!(
            bad.scanned_at_ms,
            Some(good_scanned),
            "scannedAtMs must keep describing the listing, not the failed attempt"
        );
        assert!(
            bad.checked_at_ms >= good.checked_at_ms,
            "checkedAtMs is the field that moves when an attempt fails"
        );

        // And the response says so, so a UI can render "cannot read the
        // library" rather than "no scripts".
        let v: serde_json::Value = serde_json::from_slice(&index_json(Some(&library))).unwrap();
        assert_eq!(v["configured"], true);
        assert_eq!(v["scan"], "failed");
        assert_eq!(v["scripts"].as_array().unwrap().len(), 1);

        // The socket message carries the state too, so a client that only
        // listens there does not read a failure as silence.
        let m: serde_json::Value = serde_json::from_str(&change_message(&bad)).unwrap();
        assert_eq!(m["scan"], "failed");
        assert_eq!(m["count"], 1);
    }

    /// Make `read_dir` fail while `stat` keeps working, or return `false`.
    ///
    /// Windows: deny the list-directory right (`RD`) to the current user.
    /// Unix: `0o111` — traversable and stat-able, not listable.
    #[cfg(any(windows, unix))]
    fn deny_listing(dir: &Path) -> bool {
        #[cfg(windows)]
        {
            let user = std::env::var("USERNAME").unwrap_or_default();
            std::process::Command::new("icacls")
                .arg(dir)
                .arg("/deny")
                .arg(format!("{user}:(RD)"))
                .output()
                .is_ok()
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o111)).is_ok()
        }
    }

    #[cfg(any(windows, unix))]
    fn restore_listing(dir: &Path) {
        #[cfg(windows)]
        {
            let user = std::env::var("USERNAME").unwrap_or_default();
            let _ = std::process::Command::new("icacls")
                .arg(dir)
                .arg("/remove:d")
                .arg(&user)
                .output();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755));
        }
    }

    /// **A directory that stats but will not list must be reported as failed.**
    ///
    /// This is the one the whole `ScanState` machinery was unreachable for. The
    /// gate deciding *whether to scan* used to be
    /// `dir_mtime != last || due || dir_mtime.is_none()` — a stat-level proxy
    /// for a read-level failure. A directory whose mtime has not moved and which
    /// still stats fine, but whose `read_dir` is refused, matched no clause: no
    /// scan ran, no verdict was produced, `state` stayed `Ok` and `checkedAtMs`
    /// froze for up to `FULL_RESCAN`.
    ///
    /// **Removing the directory does not test this** — that trips
    /// `is_none()` and the old gate handles it, so a test built on `remove_dir`
    /// passes against both implementations. It was written that way first. The
    /// permission denial is what distinguishes them, and it is the shape a
    /// partial SMB enumeration takes: the directory is fine, reading it is not.
    ///
    /// If the denial does not take effect — running as root, an exotic
    /// filesystem — this **fails rather than skipping**, because a silent skip
    /// here leaves the gate unguarded and reports `ok`.
    #[tokio::test]
    async fn a_directory_that_stats_but_will_not_list_is_reported_as_failed() {
        let dir = temp_dir("deny-list");
        write(&dir, "only.funscript", "{}");

        let library = Library::spawn(dir.clone());
        let mut rx = library.subscribe();
        tokio::time::timeout(Duration::from_secs(5), rx.changed())
            .await
            .expect("the first scan should land")
            .unwrap();
        assert_eq!(rx.borrow_and_update().state, ScanState::Ok);

        assert!(
            deny_listing(&dir),
            "could not deny the list-directory right, so this test cannot \
             exercise the gate it exists for"
        );

        // The precondition is the entire point of the test, so it is asserted
        // rather than assumed: stat must still succeed, and only listing fail.
        let stats = std::fs::metadata(&dir).is_ok();
        let lists = std::fs::read_dir(&dir).is_ok();
        if !stats || lists {
            restore_listing(&dir);
            panic!(
                "needed stat-ok and list-denied; got stat={stats} list_ok={lists}. \
                 Running as root, or the filesystem ignores the denial."
            );
        }

        let result = tokio::time::timeout(DIR_POLL * 4, async {
            loop {
                if rx.changed().await.is_err() {
                    panic!("channel closed");
                }
                if rx.borrow_and_update().state == ScanState::Failed {
                    return;
                }
            }
        })
        .await;

        let index = library.current();
        restore_listing(&dir);

        assert!(
            result.is_ok(),
            "an unlistable directory went on reporting scan={:?} with checkedAtMs \
             frozen — the gate skipped the scan, so the verdict that would have \
             said `failed` was never produced",
            index.state
        );
        assert_eq!(
            index.scripts.len(),
            1,
            "the previous listing is still the honest answer"
        );
    }

    /// "Nothing has been scanned yet" is its own state, not an empty listing.
    ///
    /// Without it, the first ~2 s of every start is indistinguishable from a
    /// genuinely empty directory — and the age of a snapshot that was never
    /// taken came out as `1785312003754`, i.e. fifty-six years.
    #[test]
    fn an_index_that_was_never_scanned_has_no_age() {
        let dir = temp_dir("pending");
        let (_tx, rx) = watch::channel(Arc::new(Index::default()));
        let library = Library { root: dir, rx };

        let v: serde_json::Value = serde_json::from_slice(&index_json(Some(&library))).unwrap();
        assert_eq!(v["configured"], true, "a path is set");
        assert_eq!(v["scan"], "pending", "but nothing has been read yet");
        assert!(v["scannedAtMs"].is_null());
        assert!(
            v["ageMs"].is_null(),
            "a snapshot that was never taken has no age; it is not 56 years old"
        );
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
        assert!(
            v["ageMs"].is_null(),
            "an unconfigured bridge has no snapshot to age"
        );
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
        assert_eq!(v["scan"], "ok");
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
        assert_eq!(v["scan"], "ok");
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
