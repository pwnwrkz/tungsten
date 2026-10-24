//! Lightweight startup trace for perf analysis.
//!
//! Enabled with `TUNGSTEN_PERF=1`; prints elapsed-since-start markers to
//! stderr so stdout (CLI output) is untouched.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();
static FIRST_WRITE_DONE: AtomicBool = AtomicBool::new(false);
static ENABLED: OnceLock<bool> = OnceLock::new();

/// True once for the first asset-file write of the process (debug/studio copy).
pub fn mark_first_write() {
    let is_first = !FIRST_WRITE_DONE.swap(true, Ordering::Relaxed);
    if is_first {
        mark("first asset file write");
    }
}

pub fn mark(label: &str) {
    let enabled = ENABLED.get_or_init(|| std::env::var_os("TUNGSTEN_PERF").is_some());
    if !*enabled {
        return;
    }
    let start = *START.get_or_init(Instant::now);
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    eprintln!("[perf] {ms:>9.2}ms  {label}");
}

pub fn start() {
    START.set(Instant::now()).ok();
}
