//! Diagnostic capture: timestamped input events + per-tick engine output.
//!
//! Purpose: investigate why output feels like it can't keep up with input
//! despite 40Hz input arrival on a 10Hz device packet rate. Records every
//! parsed T-Code command and every device-loop tick to a CSV with
//! microsecond-precision monotonic offsets, so analysis can correlate
//! input arrival → engine response.
//!
//! Key behaviors:
//! - Capture works without a device connected. The device loop normally
//!   bails when disconnected; while diagnostic is active it still runs the
//!   engine and records the would-be output, just skipping the BLE write.
//! - Auto-stops after `duration_ms`. Manual stop also flushes.
//! - One CSV per session at `<app_data>/diagnostic-<unix_ms>.csv`.
//! - Hot path uses `is_enabled()` (atomic load) so the cost when off is a
//!   single relaxed atomic read per input + per tick.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::{log_error, log_info};

static ENABLED: AtomicBool = AtomicBool::new(false);
static STATE: OnceLock<Mutex<Option<DiagnosticSession>>> = OnceLock::new();
static OUTPUT_DIR: OnceLock<PathBuf> = OnceLock::new();

fn state() -> &'static Mutex<Option<DiagnosticSession>> {
    STATE.get_or_init(|| Mutex::new(None))
}

/// Initialize with the app data directory. Captures will be written here.
pub fn init(dir: PathBuf) {
    let _ = OUTPUT_DIR.set(dir);
}

#[derive(Debug)]
struct DiagnosticSession {
    start_instant: Instant,
    start_wall_ms: u64,
    duration_ms: u64,
    events: Vec<Event>,
    output_path: PathBuf,
}

#[derive(Debug)]
enum Event {
    Input {
        t_us: u64,
        axis: String,
        value: f64,
        interval_ms: Option<u32>,
    },
    Tick {
        t_us: u64,
        connected: bool,
        intensity_a: u8,
        intensity_b: u8,
        wa: [u8; 4],
        wb: [u8; 4],
        /// Raw per-slot engine output (0-200) BEFORE WaveformData
        /// normalization. This is the actual signal the engine produced;
        /// `wa`/`wb` are normalized relative to packet max.
        raw_a: [u8; 4],
        raw_b: [u8; 4],
        freq_a: f64,
        freq_b: f64,
    },
}

/// Status returned to the frontend.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DiagnosticStatus {
    pub active: bool,
    pub elapsed_ms: u64,
    pub duration_ms: u64,
    pub event_count: usize,
    pub output_path: Option<String>,
}

/// Cheap hot-path gate. Single relaxed load.
#[inline]
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Begin a capture session. Returns the planned output file path.
/// Errors if a session is already active or output dir is unset.
pub fn start(duration_ms: u64) -> Result<PathBuf, String> {
    if is_enabled() {
        return Err("Diagnostic capture already active".to_string());
    }

    let dir = OUTPUT_DIR
        .get()
        .ok_or("Diagnostic output dir not initialized")?
        .clone();

    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Err(format!("Failed to create diagnostic dir: {}", e));
    }

    let start_wall_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let output_path = dir.join(format!("diagnostic-{}.csv", start_wall_ms));

    let session = DiagnosticSession {
        start_instant: Instant::now(),
        start_wall_ms,
        duration_ms,
        // Reserve enough for ~60s of 60Hz input + 10Hz ticks
        events: Vec::with_capacity(4_500),
        output_path: output_path.clone(),
    };

    {
        let mut guard = state().lock().map_err(|e| e.to_string())?;
        *guard = Some(session);
    }
    ENABLED.store(true, Ordering::Release);

    log_info!(
        "Diagnostic capture started ({}ms) -> {}",
        duration_ms,
        output_path.display()
    );

    // Auto-stop timer
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(duration_ms)).await;
        if is_enabled() {
            let _ = stop();
        }
    });

    Ok(output_path)
}

/// Stop the active session and flush events to CSV. Returns the file path.
pub fn stop() -> Result<PathBuf, String> {
    if !is_enabled() {
        return Err("No diagnostic capture is active".to_string());
    }
    ENABLED.store(false, Ordering::Release);

    let session = {
        let mut guard = state().lock().map_err(|e| e.to_string())?;
        guard.take().ok_or("Diagnostic session missing")?
    };

    let path = session.output_path.clone();
    let event_count = session.events.len();

    if let Err(e) = flush_csv(&session) {
        log_error!("Diagnostic flush failed: {}", e);
        return Err(e);
    }

    log_info!(
        "Diagnostic capture stopped: {} events written to {}",
        event_count,
        path.display()
    );
    Ok(path)
}

/// Read-only status snapshot for the frontend.
pub fn status() -> DiagnosticStatus {
    let active = is_enabled();
    let guard = match state().lock() {
        Ok(g) => g,
        Err(_) => {
            return DiagnosticStatus {
                active: false,
                elapsed_ms: 0,
                duration_ms: 0,
                event_count: 0,
                output_path: None,
            };
        }
    };

    if let Some(s) = guard.as_ref() {
        DiagnosticStatus {
            active,
            elapsed_ms: s.start_instant.elapsed().as_millis() as u64,
            duration_ms: s.duration_ms,
            event_count: s.events.len(),
            output_path: Some(s.output_path.display().to_string()),
        }
    } else {
        DiagnosticStatus {
            active: false,
            elapsed_ms: 0,
            duration_ms: 0,
            event_count: 0,
            output_path: None,
        }
    }
}

/// Record a parsed T-Code command. Cheap no-op when disabled.
pub fn record_input(axis: &str, value: f64, interval_ms: Option<u32>) {
    if !is_enabled() {
        return;
    }
    let Ok(mut guard) = state().lock() else { return };
    if let Some(s) = guard.as_mut() {
        let t_us = s.start_instant.elapsed().as_micros() as u64;
        s.events.push(Event::Input {
            t_us,
            axis: axis.to_string(),
            value,
            interval_ms,
        });
    }
}

/// Record a device-loop tick output. Cheap no-op when disabled.
///
/// `wa`/`wb` are post-normalization (0-100, relative to packet max).
/// `raw_a`/`raw_b` are pre-normalization (0-200, absolute device units) —
/// the actual engine output. Capture both so analysis can use whichever
/// matches what the device actually sees and what the engine truly emits.
pub fn record_tick(
    connected: bool,
    intensity_a: u8,
    intensity_b: u8,
    wa: [u8; 4],
    wb: [u8; 4],
    raw_a: [u8; 4],
    raw_b: [u8; 4],
    freq_a: f64,
    freq_b: f64,
) {
    if !is_enabled() {
        return;
    }
    let Ok(mut guard) = state().lock() else { return };
    if let Some(s) = guard.as_mut() {
        let t_us = s.start_instant.elapsed().as_micros() as u64;
        s.events.push(Event::Tick {
            t_us,
            connected,
            intensity_a,
            intensity_b,
            wa,
            wb,
            raw_a,
            raw_b,
            freq_a,
            freq_b,
        });
    }
}

fn flush_csv(s: &DiagnosticSession) -> Result<(), String> {
    let file =
        File::create(&s.output_path).map_err(|e| format!("Failed to create CSV: {}", e))?;
    let mut w = BufWriter::new(file);

    writeln!(
        w,
        "# Coyote diagnostic capture. start_wall_ms={}, duration_ms={}, events={}",
        s.start_wall_ms,
        s.duration_ms,
        s.events.len()
    )
    .map_err(|e| e.to_string())?;
    writeln!(
        w,
        "ts_us,kind,axis,value,interval_ms,connected,intensity_a,intensity_b,wa0,wa1,wa2,wa3,wb0,wb1,wb2,wb3,raw_a0,raw_a1,raw_a2,raw_a3,raw_b0,raw_b1,raw_b2,raw_b3,freq_a,freq_b"
    )
    .map_err(|e| e.to_string())?;

    for ev in &s.events {
        match ev {
            Event::Input {
                t_us,
                axis,
                value,
                interval_ms,
            } => {
                let interval = interval_ms
                    .map(|v| v.to_string())
                    .unwrap_or_default();
                // Trailing commas pad to match the wider tick-row schema
                // (8 extra columns: 4×raw_a + 4×raw_b).
                writeln!(
                    w,
                    "{},input,{},{:.6},{},,,,,,,,,,,,,,,,,,,,,",
                    t_us, axis, value, interval
                )
                .map_err(|e| e.to_string())?;
            }
            Event::Tick {
                t_us,
                connected,
                intensity_a,
                intensity_b,
                wa,
                wb,
                raw_a,
                raw_b,
                freq_a,
                freq_b,
            } => {
                writeln!(
                    w,
                    "{},tick,,,,{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{:.3},{:.3}",
                    t_us,
                    if *connected { 1 } else { 0 },
                    intensity_a,
                    intensity_b,
                    wa[0],
                    wa[1],
                    wa[2],
                    wa[3],
                    wb[0],
                    wb[1],
                    wb[2],
                    wb[3],
                    raw_a[0],
                    raw_a[1],
                    raw_a[2],
                    raw_a[3],
                    raw_b[0],
                    raw_b[1],
                    raw_b[2],
                    raw_b[3],
                    freq_a,
                    freq_b
                )
                .map_err(|e| e.to_string())?;
            }
        }
    }
    w.flush().map_err(|e| e.to_string())?;
    Ok(())
}
