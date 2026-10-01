//! Settings: declared by features, kept in a TOML file next to the DLL, and reloaded whenever the
//! file changes.
//!
//! A feature declares each setting as a `static` (e.g. a [`Toggle`]) that it reads directly, and
//! lists it in [`Feature::settings`](crate::Feature::settings). Every time the file is read, settings
//! missing from it are appended with their description and default value, so players can find the
//! options, including ones added in later versions.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

use toml::de::{DeTable, DeValue};

use crate::{Result, log};

/// How often the file is checked for changes (more often in tests, which wait for it).
const WATCH_INTERVAL: Duration = if cfg!(test) {
    Duration::from_millis(10)
} else {
    Duration::from_millis(500)
};

/// A setting in the settings file.
pub trait Setting: Sync {
    /// Its key in the file.
    fn name(&self) -> &'static str;
    /// Written above the setting in the file, as comment lines.
    fn description(&self) -> &'static str;
    /// The default value, as TOML.
    fn default_toml(&self) -> String;
    /// The current value, as TOML.
    fn current_toml(&self) -> String;
    /// Sets the value from the file. On error, returns what the value should be, e.g. "true or
    /// false".
    fn set(&self, value: &Value) -> std::result::Result<(), &'static str>;
    /// Sets the default value.
    fn reset(&self);
}

/// A value in the settings file.
#[derive(Debug, PartialEq)]
pub enum Value<'a> {
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(&'a str),
    /// Something else, e.g. an array or a date.
    Other,
}

impl<'a> Value<'a> {
    fn from_toml(value: &'a DeValue<'a>) -> Self {
        match value {
            DeValue::Boolean(on) => Value::Bool(*on),
            DeValue::Integer(integer) => {
                i64::from_str_radix(&integer.as_str().replace('_', ""), integer.radix())
                    .map_or(Value::Other, Value::Integer)
            }
            DeValue::Float(float) => float
                .as_str()
                .replace('_', "")
                .parse()
                .map_or(Value::Other, Value::Float),
            DeValue::String(string) => Value::String(string),
            _ => Value::Other,
        }
    }
}

/// A setting that is on or off.
pub struct Toggle {
    name: &'static str,
    description: &'static str,
    default: bool,
    value: AtomicBool,
}

impl Toggle {
    pub const fn new(name: &'static str, default: bool, description: &'static str) -> Self {
        Toggle {
            name,
            description,
            default,
            value: AtomicBool::new(default),
        }
    }

    pub fn get(&self) -> bool {
        self.value.load(Ordering::Relaxed)
    }
}

impl Setting for Toggle {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn default_toml(&self) -> String {
        self.default.to_string()
    }

    fn current_toml(&self) -> String {
        self.get().to_string()
    }

    fn set(&self, value: &Value) -> std::result::Result<(), &'static str> {
        let Value::Bool(on) = *value else {
            return Err("true or false");
        };
        self.value.store(on, Ordering::Relaxed);
        Ok(())
    }

    fn reset(&self) {
        self.value.store(self.default, Ordering::Relaxed);
    }
}

/// The settings file, and the settings in it.
struct File {
    path: PathBuf,
    /// Written at the top of a new file.
    header: String,
    settings: Vec<&'static dyn Setting>,
    /// The file's version (see `file_version`) from just before `init` read it. The watcher starts
    /// from it, so that it also reloads changes made before it started.
    initial_version: Option<(SystemTime, u64)>,
}

static FILE: Mutex<Option<Arc<File>>> = Mutex::new(None);
static WATCHER: Mutex<Option<Watcher>> = Mutex::new(None);

/// The thread reloading the settings when the file changes.
struct Watcher {
    thread: JoinHandle<()>,
    /// Set to stop the thread, which waits on the condition variable between checks.
    stop: Arc<(Mutex<bool>, Condvar)>,
}

/// Loads `settings` from the file at `path`. Fails only if two settings have the same name.
pub(crate) fn init(
    path: &Path,
    title: &str,
    homepage: &str,
    settings: Vec<&'static dyn Setting>,
) -> Result<()> {
    for (i, setting) in settings.iter().enumerate() {
        if settings[..i]
            .iter()
            .any(|other| other.name() == setting.name())
        {
            return Err(format!("two settings are named `{}`", setting.name()));
        }
    }
    let file = File {
        path: path.to_owned(),
        header: format!(
            "# {title} settings. Changes apply while the game is running.\n\
             # Settings missing from this file are added back with their default values.\n\
             # More about the mod: {homepage}\n"
        ),
        settings,
        initial_version: file_version(path),
    };
    if !load(&file) {
        file.settings.iter().for_each(|setting| setting.reset());
    }
    log::info(&format!(
        "settings: {}",
        describe(file.settings.iter().copied())
    ));
    *FILE.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(file));
    Ok(())
}

/// Starts reloading the settings in the background whenever the file changes.
pub(crate) fn start_watching() {
    let mut watcher = WATCHER.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(file) = FILE.lock().unwrap_or_else(PoisonError::into_inner).clone() else {
        return;
    };
    if watcher.is_some() {
        return;
    }
    let stop = Arc::new((Mutex::new(false), Condvar::new()));
    let thread_stop = Arc::clone(&stop);
    let spawned = thread::Builder::new()
        .name("modkit settings".into())
        .spawn(move || watch(&file, &thread_stop));
    match spawned {
        Ok(thread) => *watcher = Some(Watcher { thread, stop }),
        Err(error) => log::warn(&format!(
            "cannot watch the settings file for changes: {error}"
        )),
    }
}

/// Stops the background reloading, and waits until its thread has exited.
pub(crate) fn stop_watching() {
    let Some(watcher) = WATCHER
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
    else {
        return;
    };
    let (stopped, wake) = &*watcher.stop;
    *stopped.lock().unwrap_or_else(PoisonError::into_inner) = true;
    wake.notify_all();
    let _ = watcher.thread.join();
}

fn watch(file: &File, stop: &(Mutex<bool>, Condvar)) {
    let (stopped, wake) = stop;
    let mut last = file.initial_version;
    loop {
        let stopped = stopped.lock().unwrap_or_else(PoisonError::into_inner);
        let (stopped, _) = wake
            .wait_timeout_while(stopped, WATCH_INTERVAL, |stopped| !*stopped)
            .unwrap_or_else(PoisonError::into_inner);
        if *stopped {
            return;
        }
        drop(stopped);
        let version = file_version(&file.path);
        if version == last {
            continue;
        }
        last = version;
        let before: Vec<String> = file
            .settings
            .iter()
            .map(|setting| setting.current_toml())
            .collect();
        // An invalid file (e.g. saved halfway through an edit) keeps the settings as they were.
        load(file);
        let changed = file
            .settings
            .iter()
            .zip(&before)
            .filter(|(setting, before)| setting.current_toml() != **before);
        let changed: Vec<&dyn Setting> = changed.map(|(setting, _)| *setting).collect();
        if !changed.is_empty() {
            log::info(&format!(
                "settings changed: {}",
                describe(changed.into_iter())
            ));
        }
    }
}

fn describe<'a>(settings: impl Iterator<Item = &'a dyn Setting>) -> String {
    settings
        .map(|setting| format!("{} = {}", setting.name(), setting.current_toml()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Changes whenever the file is written, created or removed.
fn file_version(path: &Path) -> Option<(SystemTime, u64)> {
    let metadata = fs::metadata(path).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

/// Reads the settings, adding missing ones to the file. Returns false (with the reason logged, and
/// the settings unchanged) if the file can't be used.
fn load(file: &File) -> bool {
    let path = &file.path;
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            log::warn(&format!("cannot read {}: {error}", path.display()));
            return false;
        }
    };
    let present = match apply(&text, &file.settings) {
        Ok(present) => present,
        Err(error) => {
            log::warn(&format!(
                "{} is not valid TOML, keeping the current settings: {error}",
                path.display()
            ));
            return false;
        }
    };
    if let Some(completed) = complete(&text, &file.header, &file.settings, &present) {
        match fs::write(path, completed) {
            Ok(()) => log::info(&format!("added missing settings to {}", path.display())),
            Err(error) => log::warn(&format!(
                "cannot add missing settings to {}: {error}",
                path.display()
            )),
        }
    }
    true
}

/// Sets `settings` from `text`: those it doesn't set (or sets to an invalid value) get their default.
/// Returns which settings `text` sets. Changes nothing if `text` isn't valid TOML.
fn apply(
    text: &str,
    settings: &[&'static dyn Setting],
) -> std::result::Result<Vec<&'static str>, toml::de::Error> {
    // `DeTable` is the `toml` crate's serde-free document parser.
    let table = DeTable::parse(text)?;
    let table = table.get_ref();
    for key in table.keys() {
        let key: &str = key.get_ref();
        if !settings.iter().any(|setting| setting.name() == key) {
            log::warn(&format!("unknown setting `{key}`, ignoring it"));
        }
    }
    let mut present = Vec::new();
    for setting in settings {
        let value = table
            .iter()
            .find(|(key, _)| *key.get_ref() == setting.name())
            .map(|(_, value)| value.get_ref());
        let Some(value) = value else {
            setting.reset();
            continue;
        };
        present.push(setting.name());
        if let Err(expected) = setting.set(&Value::from_toml(value)) {
            log::warn(&format!(
                "`{}` must be {expected}, using the default",
                setting.name()
            ));
            setting.reset();
        }
    }
    Ok(present)
}

/// `text` with the settings it doesn't set appended, or `None` if it has them all.
fn complete(
    text: &str,
    header: &str,
    settings: &[&'static dyn Setting],
    present: &[&str],
) -> Option<String> {
    let missing: Vec<_> = settings
        .iter()
        .filter(|setting| !present.contains(&setting.name()))
        .collect();
    if missing.is_empty() {
        return None;
    }
    let mut completed = if text.trim().is_empty() {
        header.to_owned()
    } else {
        text.to_owned()
    };
    if !completed.ends_with('\n') {
        completed.push('\n');
    }
    for setting in missing {
        completed.push('\n');
        for line in setting.description().lines() {
            completed.push_str(&format!("# {line}\n"));
        }
        completed.push_str(&format!(
            "{} = {}\n",
            setting.name(),
            setting.default_toml()
        ));
    }
    Some(completed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "# Test settings.\n";

    // Each test has its own settings: tests run in parallel.
    fn test_file(path: PathBuf, settings: Vec<&'static dyn Setting>) -> File {
        File {
            path,
            header: HEADER.to_owned(),
            settings,
            initial_version: None,
        }
    }

    #[test]
    fn new_file_gets_every_setting_with_defaults() {
        static A: Toggle = Toggle::new("a", true, "Turns on A.\nOver two lines.");
        static B: Toggle = Toggle::new("b", false, "Turns on B.");
        let settings: Vec<&'static dyn Setting> = vec![&A, &B];
        let file = complete("", HEADER, &settings, &[]).unwrap();
        assert_eq!(
            file,
            "# Test settings.\n\n# Turns on A.\n# Over two lines.\na = true\n\n# Turns on B.\nb = false\n"
        );
        B.set(&Value::Bool(true)).unwrap();
        let present = apply(&file, &settings).unwrap();
        assert!(A.get() && !B.get());
        assert_eq!(present, ["a", "b"]);
        assert_eq!(complete(&file, HEADER, &settings, &present), None);
    }

    #[test]
    fn keeps_overrides_and_appends_missing_settings() {
        static A: Toggle = Toggle::new("a", true, "A.");
        static B: Toggle = Toggle::new("b", true, "B.");
        let settings: Vec<&'static dyn Setting> = vec![&A, &B];
        let text = "# mine\na = false";
        let present = apply(text, &settings).unwrap();
        assert!(!A.get() && B.get());
        let file = complete(text, HEADER, &settings, &present).unwrap();
        assert_eq!(file, "# mine\na = false\n\n# B.\nb = true\n");
    }

    #[test]
    fn invalid_values_use_defaults_and_count_as_present() {
        static A: Toggle = Toggle::new("a", true, "A.");
        let settings: Vec<&'static dyn Setting> = vec![&A];
        A.set(&Value::Bool(false)).unwrap();
        assert_eq!(apply("a = \"no\"\nunknown = 1", &settings).unwrap(), ["a"]);
        assert!(A.get());
        A.set(&Value::Bool(false)).unwrap();
        assert!(apply("a = ", &settings).is_err());
        assert!(!A.get(), "invalid TOML changes nothing");
    }

    #[test]
    fn values_are_converted_from_toml() {
        let table = DeTable::parse("b = true\ni = 0x1_F\nf = 1_000.5\ns = 'x'\na = [1]").unwrap();
        let value = |name: &str| {
            let (_, value) = table
                .get_ref()
                .iter()
                .find(|(key, _)| *key.get_ref() == name)
                .unwrap();
            Value::from_toml(value.get_ref())
        };
        assert_eq!(value("b"), Value::Bool(true));
        assert_eq!(value("i"), Value::Integer(31));
        assert_eq!(value("f"), Value::Float(1000.5));
        assert_eq!(value("s"), Value::String("x"));
        assert_eq!(value("a"), Value::Other);
    }

    #[test]
    fn load_creates_then_completes_the_file() {
        static A: Toggle = Toggle::new("a", true, "A.");
        static B: Toggle = Toggle::new("b", true, "B.");
        let dir = std::env::temp_dir().join(format!("modkit_settings_test_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = test_file(dir.join("test.toml"), vec![&A, &B]);
        let _ = fs::remove_file(&file.path);

        assert!(load(&file));
        let created = fs::read_to_string(&file.path).unwrap();
        assert!(load(&file));
        assert_eq!(
            fs::read_to_string(&file.path).unwrap(),
            created,
            "a complete file is left alone"
        );

        fs::write(&file.path, "b = false\n").unwrap();
        assert!(load(&file));
        assert!(A.get() && !B.get());
        assert_eq!(
            fs::read_to_string(&file.path).unwrap(),
            "b = false\n\n# A.\na = true\n"
        );

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_file_that_cant_be_read_changes_nothing() {
        static A: Toggle = Toggle::new("a", true, "A.");
        let dir = std::env::temp_dir().join(format!("modkit_settings_dir_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        A.set(&Value::Bool(false)).unwrap();
        // A folder where the file should be.
        assert!(!load(&test_file(dir.clone(), vec![&A])));
        assert!(!A.get());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn settings_are_described_with_their_values() {
        static A: Toggle = Toggle::new("a", true, "A.");
        static B: Toggle = Toggle::new("b", false, "B.");
        let settings: [&dyn Setting; 2] = [&A, &B];
        assert_eq!(describe(settings.into_iter()), "a = true, b = false");
    }

    /// The only test that uses the global settings file, so that tests running in parallel don't
    /// replace it.
    #[test]
    fn init_loads_the_file_and_watching_reloads_it() {
        static A: Toggle = Toggle::new("a", true, "A.");
        static B: Toggle = Toggle::new("b", false, "B.");
        let dir = std::env::temp_dir().join(format!("modkit_settings_init_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.toml");
        let _ = fs::remove_file(&path);
        let init = |settings: Vec<&'static dyn Setting>| {
            super::init(&path, "Test mod", "https://example.com/mod", settings)
        };

        assert!(init(vec![&A, &B, &A]).is_err(), "two are named a");
        init(vec![&A, &B]).unwrap();
        let created = fs::read_to_string(&path).unwrap();
        assert!(created.starts_with("# Test mod settings."), "{created}");
        assert!(created.contains("https://example.com/mod"), "{created}");

        // An invalid file is left alone, and the settings get their defaults.
        fs::write(&path, "a = ").unwrap();
        A.set(&Value::Bool(false)).unwrap();
        init(vec![&A, &B]).unwrap();
        assert!(A.get());
        assert_eq!(fs::read_to_string(&path).unwrap(), "a = ");

        let wait_for = |done: &dyn Fn() -> bool| {
            let start = std::time::Instant::now();
            while !done() && start.elapsed() < Duration::from_secs(10) {
                thread::sleep(WATCH_INTERVAL);
            }
            done()
        };
        // Changed before watching starts, as while the mod loads the game's symbols.
        fs::write(&path, "a = false\nb = true\n").unwrap();
        start_watching();
        start_watching();
        assert!(wait_for(&|| !A.get() && B.get()), "reloaded");
        fs::write(&path, "a = true\nb = true\n\n").unwrap();
        assert!(wait_for(&|| A.get() && B.get()), "reloaded again");
        stop_watching();
        fs::write(&path, "a = false\nb = false\n# stopped\n").unwrap();
        thread::sleep(WATCH_INTERVAL * 20);
        assert!(A.get() && B.get(), "no longer watched");
        fs::remove_dir_all(&dir).unwrap();
    }
}
