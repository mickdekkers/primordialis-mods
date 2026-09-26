//! Files a mod keeps for itself, next to its DLL like its log and settings: named after the mod, so
//! that they're easy to tell apart in the game folder.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// The path every file name starts with: the mod's folder and name.
static PREFIX: Mutex<Option<PathBuf>> = Mutex::new(None);

pub(crate) fn init(home: &Path, mod_name: &str) {
    *PREFIX.lock().unwrap_or_else(PoisonError::into_inner) = Some(home.join(mod_name));
}

/// Where the mod keeps its file `<mod name>_<suffix>`, e.g. `primordialis_qol_detected.bin` for
/// `detected.bin`. `None` until the mod has started.
pub fn path(suffix: &str) -> Option<PathBuf> {
    let prefix = PREFIX.lock().unwrap_or_else(PoisonError::into_inner);
    let prefix = prefix.as_ref()?;
    let mut name = prefix.file_name()?.to_owned();
    name.push("_");
    name.push(suffix);
    Some(prefix.with_file_name(name))
}
