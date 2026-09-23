//! User settings, from `primordialis_qol.toml` next to the DLL, reloaded whenever the file changes.
//!
//! Every time the file is read, settings missing from it are appended with their description and
//! default value, so players can find the options, including ones added in later versions.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{PoisonError, RwLock};
use std::thread;
use std::time::{Duration, SystemTime};

use crate::log;

pub const FILE_NAME: &str = "primordialis_qol.toml";

const HEADER: &str = "\
# Primordialis QoL settings. Changes apply while the game is running.
# Settings missing from this file are added back with their default values.
";

/// How often the file is checked for changes.
const WATCH_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub fix_icon_positions: bool,
    pub fix_echolocation_positions: bool,
}

/// A setting as it appears in the file.
struct Setting {
    name: &'static str,
    /// Written above the setting, as comment lines.
    description: &'static str,
    field: fn(&mut Config) -> &mut bool,
}

const SETTINGS: &[Setting] = &[
    Setting {
        name: "fix_icon_positions",
        description: "\
The game only simulates cell pickups near you. Far away, a pickup can sit inside rock where it
spawned, until the game pushes it out as you get close. Show map icons where pickups will end up.",
        field: |config| &mut config.fix_icon_positions,
    },
    Setting {
        name: "fix_echolocation_positions",
        description: "\
Also show the Echolocation mutation's pickup markers where pickups will end up, like the map icons.",
        field: |config| &mut config.fix_echolocation_positions,
    },
];

impl Config {
    const DEFAULT: Config = Config { fix_icon_positions: true, fix_echolocation_positions: true };
}

impl Default for Config {
    fn default() -> Self {
        Config::DEFAULT
    }
}

static CURRENT: RwLock<Config> = RwLock::new(Config::DEFAULT);

/// The settings currently in effect.
pub fn current() -> Config {
    *CURRENT.read().unwrap_or_else(PoisonError::into_inner)
}

/// Loads the settings from `dir`, and reloads them in the background whenever the file changes.
pub fn init(dir: &Path) {
    let path = dir.join(FILE_NAME);
    let config = load(&path).unwrap_or_default();
    *CURRENT.write().unwrap_or_else(PoisonError::into_inner) = config;
    log::info(&format!("settings: {config:?}"));

    let spawned = thread::Builder::new().name("primordialis_qol settings".into()).spawn(move || watch(path));
    if let Err(error) = spawned {
        log::warn(&format!("cannot watch {FILE_NAME} for changes: {error}"));
    }
}

fn watch(path: PathBuf) {
    let mut last = file_version(&path);
    loop {
        thread::sleep(WATCH_INTERVAL);
        let version = file_version(&path);
        if version == last {
            continue;
        }
        last = version;
        // An invalid file (e.g. saved halfway through an edit) keeps the settings as they were.
        let Some(config) = load(&path) else { continue };
        let mut current = CURRENT.write().unwrap_or_else(PoisonError::into_inner);
        if *current != config {
            *current = config;
            log::info(&format!("settings changed: {config:?}"));
        }
    }
}

/// Changes whenever the file is written, created or removed.
fn file_version(path: &Path) -> Option<(SystemTime, u64)> {
    let metadata = fs::metadata(path).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

/// Reads the settings, adding missing ones to the file. `None` if the file can't be used, with the
/// reason logged.
fn load(path: &Path) -> Option<Config> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            log::warn(&format!("cannot read {}: {error}", path.display()));
            return None;
        }
    };
    let (config, present) = match parse(&text) {
        Ok(parsed) => parsed,
        Err(error) => {
            log::warn(&format!("{FILE_NAME} is not valid TOML, keeping the current settings: {error}"));
            return None;
        }
    };
    if let Some(completed) = complete(&text, &present) {
        match fs::write(path, completed) {
            Ok(()) => log::info(&format!("added missing settings to {}", path.display())),
            Err(error) => log::warn(&format!("cannot add missing settings to {}: {error}", path.display())),
        }
    }
    Some(config)
}

/// Parses the settings in `text`, and which of them it sets.
fn parse(text: &str) -> Result<(Config, Vec<&'static str>), toml::de::Error> {
    // `DeTable` is the `toml` crate's serde-free document parser.
    let table = toml::de::DeTable::parse(text)?;
    let mut config = Config::default();
    let mut present = Vec::new();
    for (key, value) in table.get_ref() {
        let key: &str = key.get_ref();
        let Some(setting) = SETTINGS.iter().find(|setting| setting.name == key) else {
            log::warn(&format!("{FILE_NAME}: unknown setting `{key}`, ignoring it"));
            continue;
        };
        present.push(setting.name);
        match value.get_ref().as_bool() {
            Some(on) => *(setting.field)(&mut config) = on,
            None => log::warn(&format!("{FILE_NAME}: `{key}` must be true or false, using the default")),
        }
    }
    Ok((config, present))
}

/// `text` with the settings it doesn't set appended, or `None` if it has them all.
fn complete(text: &str, present: &[&str]) -> Option<String> {
    let missing: Vec<&Setting> = SETTINGS.iter().filter(|setting| !present.contains(&setting.name)).collect();
    if missing.is_empty() {
        return None;
    }
    let mut completed = if text.trim().is_empty() { HEADER.to_owned() } else { text.to_owned() };
    if !completed.ends_with('\n') {
        completed.push('\n');
    }
    let mut defaults = Config::default();
    for setting in missing {
        completed.push('\n');
        for line in setting.description.lines() {
            completed.push_str(&format!("# {line}\n"));
        }
        completed.push_str(&format!("{} = {}\n", setting.name, *(setting.field)(&mut defaults)));
    }
    Some(completed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_file_gets_every_setting_with_defaults() {
        let file = complete("", &[]).unwrap();
        assert!(file.starts_with(HEADER));
        let (config, present) = parse(&file).unwrap();
        assert_eq!(config, Config::default());
        assert_eq!(present.len(), SETTINGS.len());
        assert_eq!(complete(&file, &present), None);
    }

    #[test]
    fn keeps_overrides_and_appends_missing_settings() {
        let text = "# mine\nfix_icon_positions = false";
        let (config, present) = parse(text).unwrap();
        assert!(!config.fix_icon_positions);
        let file = complete(text, &present).unwrap();
        assert!(file.starts_with("# mine\nfix_icon_positions = false\n"));
        assert!(file.contains("\nfix_echolocation_positions = true\n"));
        let (config, _) = parse(&file).unwrap();
        assert!(!config.fix_icon_positions && config.fix_echolocation_positions);
    }

    #[test]
    fn load_creates_then_completes_the_file() {
        let dir = std::env::temp_dir().join(format!("primordialis_qol_config_test_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE_NAME);
        let _ = fs::remove_file(&path);

        assert_eq!(load(&path), Some(Config::default()));
        let created = fs::read_to_string(&path).unwrap();
        assert_eq!(load(&path), Some(Config::default()));
        assert_eq!(fs::read_to_string(&path).unwrap(), created, "a complete file is left alone");

        fs::write(&path, "fix_echolocation_positions = false\n").unwrap();
        assert!(!load(&path).unwrap().fix_echolocation_positions);
        let completed = fs::read_to_string(&path).unwrap();
        assert!(completed.starts_with("fix_echolocation_positions = false\n"));
        assert!(completed.contains("\nfix_icon_positions = true\n"));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn invalid_values_use_defaults_and_count_as_present() {
        let (config, present) = parse("fix_echolocation_positions = \"no\"").unwrap();
        assert!(config.fix_echolocation_positions);
        assert_eq!(present, ["fix_echolocation_positions"]);
        assert!(parse("fix_icon_positions = ").is_err());
    }
}
