//! Minimal file logger. The log is recreated on every launch, next to the DLL.

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static LOG: Mutex<Option<File>> = Mutex::new(None);

pub fn init(path: &Path) {
    if let Ok(mut log) = LOG.lock() {
        *log = File::create(path).ok();
    }
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
    let Ok(mut log) = LOG.lock() else { return };
    let Some(file) = log.as_mut() else { return };
    let seconds = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
    let _ = writeln!(file, "[{seconds:.3}] {level}: {message}");
    let _ = file.flush();
}
