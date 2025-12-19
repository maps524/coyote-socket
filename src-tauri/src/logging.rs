//! Simple ring buffer logger that maintains the last N lines in a log file.
//! Designed for development use - allows agents to read recent logs.

use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

const MAX_LINES: usize = 1000;
const FLUSH_INTERVAL: usize = 10; // Flush to disk every N log calls

static LOGGER: OnceLock<Mutex<RingLogger>> = OnceLock::new();

pub struct RingLogger {
    buffer: VecDeque<String>,
    log_path: PathBuf,
    write_count: usize,
}

impl RingLogger {
    fn new(log_path: PathBuf) -> Self {
        Self {
            buffer: VecDeque::with_capacity(MAX_LINES),
            log_path,
            write_count: 0,
        }
    }

    fn log(&mut self, level: &str, message: &str) {
        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);

        let line = format!("[{timestamp}] [{level}] {message}");

        // Add to ring buffer
        if self.buffer.len() >= MAX_LINES {
            self.buffer.pop_front();
        }
        self.buffer.push_back(line);

        // Periodic flush
        self.write_count += 1;
        if self.write_count >= FLUSH_INTERVAL {
            self.flush();
            self.write_count = 0;
        }
    }

    fn flush(&self) {
        if let Ok(mut file) = File::create(&self.log_path) {
            for line in &self.buffer {
                let _ = writeln!(file, "{}", line);
            }
        }
    }
}

/// Initialize the logger. Call once at startup.
/// Log file will be created at `<app_dir>/coyote-socket.log`
pub fn init_logger(app_dir: Option<PathBuf>) {
    let log_path = app_dir
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
        .join("coyote-socket.log");

    // Clear any existing log file
    let _ = fs::remove_file(&log_path);

    let logger = RingLogger::new(log_path);
    let _ = LOGGER.set(Mutex::new(logger));
}

/// Log a message at the specified level
pub fn log(level: &str, message: &str) {
    if let Some(logger) = LOGGER.get() {
        if let Ok(mut guard) = logger.lock() {
            guard.log(level, message);
        }
    }

    // Also print to console in debug builds
    #[cfg(debug_assertions)]
    eprintln!("[{level}] {message}");
}

/// Flush the log buffer to disk immediately
pub fn flush() {
    if let Some(logger) = LOGGER.get() {
        if let Ok(guard) = logger.lock() {
            guard.flush();
        }
    }
}

/// Get the path to the log file
pub fn get_log_path() -> Option<PathBuf> {
    LOGGER.get().and_then(|logger| {
        logger.lock().ok().map(|guard| guard.log_path.clone())
    })
}

// Convenience macros
#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {
        $crate::logging::log("INFO", &format!($($arg)*))
    };
}

#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => {
        $crate::logging::log("WARN", &format!($($arg)*))
    };
}

#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => {
        $crate::logging::log("ERROR", &format!($($arg)*))
    };
}

#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => {
        $crate::logging::log("DEBUG", &format!($($arg)*))
    };
}
