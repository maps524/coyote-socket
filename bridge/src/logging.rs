//! Ring buffer logger — a trimmed copy of `src-tauri/src/logging.rs`.
//!
//! Deliberately API-compatible with the desktop app's logger (same
//! `log_info!` / `log_warn!` / `log_error!` / `log_debug!` macros, same
//! `[timestamp_ms] [LEVEL] message` line format) so that when this repo is
//! hollowed out into the bridge the two collapse into one file.
//!
//! The one thing removed is the `crate::emit_backend_log` call, which pushes
//! lines to the Tauri frontend. The bridge has no frontend to push to, so
//! lines go to stderr instead. That call site is the only coupling the
//! desktop logger has to Tauri.

use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

const MAX_LINES: usize = 1000;
const FLUSH_INTERVAL: usize = 10;

static LOGGER: OnceLock<Mutex<RingLogger>> = OnceLock::new();

/// Live tap on the log, for a UI that wants to show it without tailing a file.
///
/// A `broadcast` rather than a callback so that a slow or absent reader can
/// never stall a log call — logging sits on the protocol path and must stay
/// non-blocking. A reader that falls behind is told how many lines it missed
/// rather than being handed a silently truncated history.
static TAP: OnceLock<tokio::sync::broadcast::Sender<String>> = OnceLock::new();

/// How many lines a reader may fall behind before it starts losing them.
/// Generous, because the interesting bursts are exactly when a connection is
/// failing and someone is watching.
const TAP_CAPACITY: usize = 512;

/// Subscribe to log lines as they are written, in the same
/// `[timestamp] [LEVEL] message` form they take on disk.
pub fn subscribe() -> tokio::sync::broadcast::Receiver<String> {
    TAP.get_or_init(|| tokio::sync::broadcast::channel(TAP_CAPACITY).0)
        .subscribe()
}

struct RingLogger {
    buffer: VecDeque<String>,
    log_path: PathBuf,
    write_count: usize,
}

impl RingLogger {
    fn log(&mut self, level: &str, message: &str) -> String {
        let line = format!("[{}] [{level}] {message}", now_ms());

        if self.buffer.len() >= MAX_LINES {
            self.buffer.pop_front();
        }
        self.buffer.push_back(line.clone());

        self.write_count += 1;
        if self.write_count >= FLUSH_INTERVAL {
            self.flush();
            self.write_count = 0;
        }
        line
    }

    fn snapshot(&self) -> Vec<String> {
        self.buffer.iter().cloned().collect()
    }

    fn flush(&self) {
        if let Ok(mut file) = File::create(&self.log_path) {
            for line in &self.buffer {
                let _ = writeln!(file, "{}", line);
            }
        }
    }
}

/// Milliseconds since the Unix epoch. Shared by the logger and by state
/// timestamps on the wire.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Initialise the logger. Writes `<dir>/coyote-bridge.log`.
pub fn init(dir: Option<PathBuf>) {
    let dir = dir.unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!("[logging] failed to create log dir {}: {e}", dir.display());
    }
    let log_path = dir.join("coyote-bridge.log");
    let _ = fs::remove_file(&log_path);
    let _ = LOGGER.set(Mutex::new(RingLogger {
        buffer: VecDeque::with_capacity(MAX_LINES),
        log_path,
        write_count: 0,
    }));
}

pub fn log(level: &str, message: &str) {
    let mut line = None;
    if let Some(logger) = LOGGER.get() {
        if let Ok(mut guard) = logger.lock() {
            line = Some(guard.log(level, message));
        }
    }
    eprintln!("[{level}] {message}");

    // Only publish if someone has ever subscribed: on the headless path this
    // channel is never created at all.
    if let Some(tx) = TAP.get() {
        let _ = tx.send(line.unwrap_or_else(|| format!("[{}] [{level}] {message}", now_ms())));
    }
}

/// The whole ring buffer, oldest first. What a "copy the log" button copies.
pub fn history() -> Vec<String> {
    LOGGER
        .get()
        .and_then(|l| l.lock().ok().map(|g| g.snapshot()))
        .unwrap_or_default()
}

/// Force the buffer to disk regardless of the flush interval.
pub fn flush_now() {
    if let Some(logger) = LOGGER.get() {
        if let Ok(guard) = logger.lock() {
            guard.flush();
        }
    }
}

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => { $crate::logging::log("INFO", &format!($($arg)*)) };
}

#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => { $crate::logging::log("WARN", &format!($($arg)*)) };
}

#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => { $crate::logging::log("ERROR", &format!($($arg)*)) };
}

#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => { $crate::logging::log("DEBUG", &format!($($arg)*)) };
}
