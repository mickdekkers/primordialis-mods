//! Minimal file logger, next to the DLL. Recreated when the game starts; appended to when the hot
//! reload host loads a new build.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

static LOG: Mutex<Option<File>> = Mutex::new(None);

pub fn init(path: &Path, append: bool) {
    let file = if append {
        OpenOptions::new().create(true).append(true).open(path)
    } else {
        File::create(path)
    };
    *LOG.lock().unwrap_or_else(PoisonError::into_inner) = file.ok();
}

/// Closes the log file; later messages are dropped.
pub fn close() {
    *LOG.lock().unwrap_or_else(PoisonError::into_inner) = None;
}

pub fn info(message: &str) {
    write("info", message);
}

pub fn warn(message: &str) {
    write("warn", message);
}

pub fn error(message: &str) {
    write("error", message);
}

fn write(level: &str, message: &str) {
    let mut log = LOG.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(file) = log.as_mut() else { return };
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let _ = writeln!(file, "[{seconds:.3}] {level}: {message}");
    let _ = file.flush();
}
