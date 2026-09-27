//! Minimal file logger, next to the DLL. Each run of the game gets a new log, and the previous one is
//! kept as `.log.bak`: after a crash, the log that saw it is still there once the game is started
//! again. Builds the hot reload host swaps in append to the log of the game they're loaded into.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use windows_sys::Win32::System::SystemInformation::GetLocalTime;

static LOG: Mutex<Option<File>> = Mutex::new(None);

/// Opens the log at `path`: the one this process started, or a new one, keeping the previous one as
/// `<path>.bak`. A log's first line names the process it belongs to, since each build the hot reload
/// host loads starts afresh and can't tell otherwise whether it's the first in this game.
pub(crate) fn init(path: &Path) {
    let header = format!("log of process {}", std::process::id());
    let ours = first_line(path).is_some_and(|line| line.ends_with(&header));
    let fresh = if ours {
        Ok(())
    } else {
        // Fails if there's no log yet, which is fine.
        let _ = fs::rename(path, path.with_extension("log.bak"));
        File::create(path).map(drop)
    };
    // Only ever appended to: while the hot reload host swaps builds, both write to the log, each
    // through its own handle, and would otherwise write over each other's lines.
    let file = fresh.and_then(|()| OpenOptions::new().append(true).open(path));
    *LOG.lock().unwrap_or_else(PoisonError::into_inner) = file.ok();
    if !ours {
        info(&header);
    }
}

/// The first line of the file at `path`, if it can be read. Only reads the start of it: a log from
/// before its first line named the process can have a long one.
fn first_line(path: &Path) -> Option<String> {
    let mut line = String::new();
    let start = File::open(path).ok()?.take(256);
    BufReader::new(start).read_line(&mut line).ok()?;
    Some(line.trim_end().to_owned())
}

/// Closes the log file; later messages are dropped.
pub(crate) fn close() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_run_of_the_game_gets_a_new_log_and_keeps_the_last_one() {
        let dir = std::env::temp_dir().join(format!("modkit_log_test_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let (path, backup) = (dir.join("test.log"), dir.join("test.log.bak"));
        // Process 0 is the system's idle process, which no game runs in.
        fs::write(&path, "[time] info: log of process 0\nthe last run\n").unwrap();
        init(&path);
        info("this run");
        // The next build opens the log while this one still writes to it.
        let mut next_build = OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(next_build, "the next build").unwrap();
        info("stopping");
        // The next build swapped in.
        init(&path);
        info("after a hot reload");
        close();
        let log = fs::read_to_string(&path).unwrap();
        let header = format!("log of process {}", std::process::id());
        assert!(log.lines().next().unwrap().ends_with(&header), "{log}");
        assert_eq!(log.matches("log of process").count(), 1, "{log}");
        for line in [
            "this run",
            "the next build",
            "stopping",
            "after a hot reload",
        ] {
            assert!(log.contains(line), "{line}: {log}");
        }
        assert!(!log.contains("the last run"));
        assert!(
            fs::read_to_string(&backup)
                .unwrap()
                .contains("the last run")
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
