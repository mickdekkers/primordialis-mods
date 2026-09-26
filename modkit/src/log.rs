//! Minimal file logger, next to the DLL. Recreated when the game starts; appended to when the hot
//! reload host loads a new build.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use windows_sys::Win32::System::SystemInformation::GetLocalTime;

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
    let _ = writeln!(file, "[{}] {level}: {message}", local_time());
    let _ = file.flush();
}

/// The time on the player's clock, so a log attached to a bug report lines up with what they saw.
fn local_time() -> String {
    // SAFETY: `GetLocalTime` only writes the struct it's given, which is plain data.
    let t = unsafe {
        let mut t = std::mem::zeroed();
        GetLocalTime(&mut t);
        t
    };
    format!(
        "{}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}
