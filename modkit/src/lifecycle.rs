//! Starting and stopping a mod: when the game loads it (`--customdll`), or when the hot reload host
//! swaps builds (`modkit_protocol`).

use std::ffi::c_void;
use std::panic;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use windows_sys::Win32::Foundation::{HMODULE, TRUE};
use windows_sys::Win32::System::LibraryLoader::DisableThreadLibraryCalls;
use windows_sys::Win32::System::SystemServices::{DLL_PROCESS_ATTACH, DLL_PROCESS_DETACH};
use windows_sys::core::BOOL;

use crate::feature::{Feature, Features};
use crate::game::bindings::Bindings;
use crate::game::symbols;
use crate::{Result, alloc, events, log, module, settings};

/// A mod: what `entry!` is given.
pub struct Mod {
    /// File name stem, for the log (`<name>.log`), settings (`<name>.toml`) and symbol cache
    /// (`<name>_cache`), next to the DLL.
    pub name: &'static str,
    /// For people, e.g. in the settings file.
    pub title: &'static str,
    pub version: &'static str,
    /// Creates the mod's features. Called once per start.
    pub features: fn() -> Vec<Box<dyn Feature>>,
}

/// Whether the mod is hooked into the game.
static RUNNING: AtomicBool = AtomicBool::new(false);
/// What `prepare` found, for `start`.
static PREPARED: Mutex<Option<Prepared>> = Mutex::new(None);

struct Prepared {
    bindings: Bindings,
    features: Features,
}

/// The mod DLL's `DllMain`.
///
/// # Safety
///
/// Only for `DllMain`, with the arguments Windows passes it.
pub unsafe fn dll_main(
    definition: &'static Mod,
    module: HMODULE,
    reason: u32,
    reserved: *mut c_void,
) -> BOOL {
    match reason {
        DLL_PROCESS_ATTACH => {
            // SAFETY: `module` is our own module handle, provided by the loader.
            unsafe { DisableThreadLibraryCalls(module) };
            if module::is_loaded(modkit_protocol::HOST_MODULE) {
                // The host calls `prepare` and `start` once loading is done.
                return TRUE;
            }
            // Loaded by the game through `--customdll`, on its main thread while parsing the command
            // line, before it renders anything. Hooking right away means the hooks are in place before
            // the game first runs the hooked functions.
            let result = panic::catch_unwind(|| {
                let path = module::path(module)?;
                let dir = path.parent().ok_or("DLL path has no parent directory")?;
                activate(prepare_in(definition, dir, false)?)
            });
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    log::error(&format!("not active, the game runs unmodified: {error}"))
                }
                Err(_) => log::error("not active, the game runs unmodified: panicked during setup"),
            }
        }
        // Unloaded with `FreeLibrary` (not process exit), which only the host does, after stopping us:
        // free everything we allocated. None of our code runs anymore.
        DLL_PROCESS_DETACH if reserved.is_null() && !RUNNING.load(Ordering::SeqCst) => {
            alloc::destroy()
        }
        _ => {}
    }
    // Never fail the load: a broken mod should degrade to an unmodified game, not a crash.
    TRUE
}

/// Everything that doesn't change the game: settings, and finding what to hook. `home` holds the log,
/// settings and symbol cache.
fn prepare_in(definition: &Mod, home: &Path, append_log: bool) -> Result<Prepared> {
    log::init(&home.join(format!("{}.log", definition.name)), append_log);
    let location = module::own()
        .and_then(module::path)
        .map(|path| path.display().to_string());
    log::info(&format!(
        "{} {} loaded from {}",
        definition.name,
        definition.version,
        location.as_deref().unwrap_or("an unknown location")
    ));
    let features = (definition.features)();
    let declared = features
        .iter()
        .flat_map(|feature| feature.settings())
        .collect();
    settings::init(
        &home.join(format!("{}.toml", definition.name)),
        definition.title,
        declared,
    )?;

    let game_exe = module::path(ptr::null_mut())?;
    let game_dir = game_exe
        .parent()
        .ok_or("game path has no parent directory")?;
    let symbols = symbols::load(game_dir, &home.join(format!("{}_cache", definition.name)))?;
    let bindings = Bindings::resolve(&symbols)?;
    Ok(Prepared {
        bindings,
        features: Features::new(features),
    })
}

/// Hooks the game. Makes no assumptions about what the game is doing: it may be starting up, or
/// already running.
fn activate(prepared: Prepared) -> Result<()> {
    let names = prepared.features.names().join(", ");
    events::start(prepared.bindings, prepared.features)?;
    settings::start_watching();
    RUNNING.store(true, Ordering::SeqCst);
    log::info(&format!("ready: {names}"));
    Ok(())
}

/// Undoes `activate`. On failure, everything keeps running as before.
fn deactivate() -> Result<()> {
    settings::stop_watching();
    if let Err(error) = events::stop() {
        settings::start_watching();
        return Err(error);
    }
    RUNNING.store(false, Ordering::SeqCst);
    Ok(())
}

/// `modkit_protocol::EXPORT_PREPARE`.
///
/// # Safety
///
/// `home_dir` must be valid for `home_dir_len` UTF-16 units. Call from one thread at a time, and not
/// from `DllMain`.
pub unsafe fn prepare(definition: &'static Mod, home_dir: *const u16, home_dir_len: usize) -> bool {
    let result = panic::catch_unwind(|| {
        // SAFETY: Guaranteed by the caller.
        let home = unsafe { std::slice::from_raw_parts(home_dir, home_dir_len) };
        let home = PathBuf::from(String::from_utf16_lossy(home));
        if RUNNING.load(Ordering::SeqCst) {
            return Err("already running".to_owned());
        }
        prepare_in(definition, &home, true)
    });
    let error = match result {
        Ok(Ok(prepared)) => {
            *PREPARED.lock().unwrap_or_else(PoisonError::into_inner) = Some(prepared);
            return true;
        }
        Ok(Err(error)) => error,
        Err(_) => "panicked while preparing".to_owned(),
    };
    log::error(&format!("not active: {error}"));
    log::close();
    false
}

/// `modkit_protocol::EXPORT_START`.
pub fn start() -> bool {
    let result = panic::catch_unwind(|| {
        let prepared = PREPARED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        activate(prepared.ok_or("not prepared")?)
    });
    let error = match result {
        Ok(Ok(())) => return true,
        Ok(Err(error)) => error,
        Err(_) => "panicked during setup".to_owned(),
    };
    log::error(&format!("not active: {error}"));
    // Nothing `activate` did before failing needs undoing: the settings watcher starts last.
    log::close();
    false
}

/// `modkit_protocol::EXPORT_STOP`.
pub fn stop() -> bool {
    match panic::catch_unwind(deactivate) {
        Ok(Ok(())) => {
            log::info("stopped: hooks removed, ready to be unloaded");
            log::close();
            true
        }
        Ok(Err(error)) => {
            log::error(&format!("cannot stop, still running: {error}"));
            false
        }
        Err(_) => {
            log::error("cannot stop, panicked while stopping");
            false
        }
    }
}
