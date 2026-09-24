//! Primordialis QoL mod: shows the icon of every cell pickup on the map, for areas you have explored.
//!
//! Loaded by the game itself through its `--customdll "primordialis_qol.dll"` launch option. On load we
//! look up the game's internals by name in the debug symbols the game ships (`pdbs.zip`), hook two
//! rendering functions, and draw the icons with the game's own icon renderer while the map is open.
//! If anything doesn't match what we expect, we log why and leave the game unmodified.
//!
//! For development, the hot reload host (`hot_reload/`) loads copies of this DLL instead, and swaps
//! them while the game runs: see `primordialis_qol_prepare`, `primordialis_qol_start` and
//! `primordialis_qol_stop`.

mod alloc;
mod config;
mod freeze;
mod game;
mod hooks;
mod log;
mod overlay;
mod symbols;

use std::ffi::c_void;
use std::panic;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use windows_sys::Win32::Foundation::{HMODULE, MAX_PATH, TRUE};
use windows_sys::Win32::System::LibraryLoader::{
    DisableThreadLibraryCalls, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    GetModuleFileNameW, GetModuleHandleExW, GetModuleHandleW,
};
use windows_sys::Win32::System::SystemServices::{DLL_PROCESS_ATTACH, DLL_PROCESS_DETACH};
use windows_sys::core::BOOL;

pub type Result<T> = std::result::Result<T, String>;

#[global_allocator]
static ALLOCATOR: alloc::PrivateHeap = alloc::PrivateHeap;

/// The hot reload host. When it's loaded, it starts and stops this module itself.
const HOST_MODULE: &str = "primordialis_qol_hot_reload.dll";
/// Version of the functions the host calls. Bumped on any incompatible change to them.
const HOST_API_VERSION: u32 = 2;
const LOG_FILE: &str = "primordialis_qol.log";

/// Whether the hooks are installed and the settings watcher runs.
static RUNNING: AtomicBool = AtomicBool::new(false);
/// What `primordialis_qol_prepare` found, for `primordialis_qol_start`.
static PREPARED: Mutex<Option<game::Layout>> = Mutex::new(None);

#[unsafe(no_mangle)]
// The signature is dictated by Windows; the loader always passes our own, valid module handle.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "system" fn DllMain(module: HMODULE, reason: u32, reserved: *mut c_void) -> BOOL {
    match reason {
        DLL_PROCESS_ATTACH => {
            // SAFETY: `module` is our own module handle, provided by the loader.
            unsafe { DisableThreadLibraryCalls(module) };
            if host_loaded() {
                // The host calls `primordialis_qol_prepare` and `primordialis_qol_start` once loading
                // is done.
                return TRUE;
            }
            // Loaded by the game through `--customdll`, on its main thread while parsing the command
            // line, before it renders anything. Hooking right away means the hooks are in place before
            // the game first runs the hooked functions.
            let result = panic::catch_unwind(|| {
                let dir = module_path(module)?.parent().ok_or("DLL path has no parent directory")?.to_path_buf();
                start(&dir, false)
            });
            match result {
                Ok(Ok(())) => log::info("ready: map icons enabled"),
                Ok(Err(error)) => log::error(&format!("not active, the game runs unmodified: {error}")),
                Err(_) => log::error("not active, the game runs unmodified: panicked during setup"),
            }
        }
        // Unloaded with `FreeLibrary` (not process exit), which only the host does, after stopping us:
        // free everything we allocated. None of our code runs anymore.
        DLL_PROCESS_DETACH if reserved.is_null() && !RUNNING.load(Ordering::SeqCst) => alloc::destroy(),
        _ => {}
    }
    // Never fail the load: a broken mod should degrade to an unmodified game, not a crash.
    TRUE
}

fn host_loaded() -> bool {
    let name: Vec<u16> = HOST_MODULE.encode_utf16().chain(Some(0)).collect();
    // SAFETY: A NUL-terminated module name; doesn't load anything.
    !unsafe { GetModuleHandleW(name.as_ptr()) }.is_null()
}

/// Sets everything up. `home` holds the log, settings and symbol cache. Makes no assumptions about
/// what the game is doing: it may be starting up, or already running.
fn start(home: &Path, append_log: bool) -> Result<()> {
    let layout = prepare(home, append_log)?;
    activate(layout)
}

/// Everything that doesn't change the game: settings, and finding what to hook.
fn prepare(home: &Path, append_log: bool) -> Result<game::Layout> {
    log::init(&home.join(LOG_FILE), append_log);
    let location = own_module().and_then(module_path).map(|path| path.display().to_string());
    log::info(&format!(
        "primordialis_qol {} loaded from {}",
        env!("CARGO_PKG_VERSION"),
        location.as_deref().unwrap_or("an unknown location")
    ));
    config::init(home);

    let game_exe = module_path(ptr::null_mut())?;
    let game_dir = game_exe.parent().ok_or("game path has no parent directory")?;
    let symbols = symbols::load(game_dir, &home.join("primordialis_qol_cache"))?;
    game::Layout::resolve(&symbols)
}

/// Hooks the game.
fn activate(layout: game::Layout) -> Result<()> {
    hooks::install(layout)?;
    config::start_watching();
    RUNNING.store(true, Ordering::SeqCst);
    Ok(())
}

/// Undoes `start`. On failure, everything keeps running as before.
fn stop() -> Result<()> {
    config::stop_watching();
    if let Err(error) = hooks::uninstall() {
        config::start_watching();
        return Err(error);
    }
    overlay::reset();
    RUNNING.store(false, Ordering::SeqCst);
    Ok(())
}

/// For the host: the version of this API.
#[unsafe(no_mangle)]
pub extern "C" fn primordialis_qol_host_api_version() -> u32 {
    HOST_API_VERSION
}

/// For the host: loads the settings and finds everything to hook, without changing the game yet, so
/// that this can happen while the previous build still runs. The log, settings and symbol cache are
/// in `home_dir` (UTF-16, `home_dir_len` units, not NUL-terminated). Returns whether
/// `primordialis_qol_start` can be called; if not, the module can be unloaded.
///
/// # Safety
///
/// `home_dir` must be valid for `home_dir_len` UTF-16 units. Call from one thread at a time, and
/// not from `DllMain`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn primordialis_qol_prepare(home_dir: *const u16, home_dir_len: usize) -> bool {
    let result = panic::catch_unwind(|| {
        // SAFETY: Guaranteed by the caller.
        let home = unsafe { std::slice::from_raw_parts(home_dir, home_dir_len) };
        let home = PathBuf::from(String::from_utf16_lossy(home));
        if RUNNING.load(Ordering::SeqCst) {
            return Err("already running".to_owned());
        }
        prepare(&home, true)
    });
    let error = match result {
        Ok(Ok(layout)) => {
            *PREPARED.lock().unwrap_or_else(PoisonError::into_inner) = Some(layout);
            return true;
        }
        Ok(Err(error)) => error,
        Err(_) => "panicked while preparing".to_owned(),
    };
    log::error(&format!("not active: {error}"));
    log::close();
    false
}

/// For the host: hooks the game, after `primordialis_qol_prepare`. Makes no assumptions about what
/// the game is doing. Returns whether the mod is active; if not, none of its code runs and the
/// module can be unloaded.
#[unsafe(no_mangle)]
pub extern "C" fn primordialis_qol_start() -> bool {
    let result = panic::catch_unwind(|| {
        let layout = PREPARED.lock().unwrap_or_else(PoisonError::into_inner).take();
        activate(layout.ok_or("not prepared")?)
    });
    let error = match result {
        Ok(Ok(())) => {
            log::info("ready: map icons enabled (loaded by the hot reload host)");
            return true;
        }
        Ok(Err(error)) => error,
        Err(_) => "panicked during setup".to_owned(),
    };
    log::error(&format!("not active: {error}"));
    // Nothing `activate` did before failing needs undoing: the settings watcher starts last.
    log::close();
    false
}

/// For the host: removes the hooks and frees everything, waiting until no thread is inside the mod.
/// Returns whether the module can now be unloaded; if not, the mod keeps running.
#[unsafe(no_mangle)]
pub extern "C" fn primordialis_qol_stop() -> bool {
    match panic::catch_unwind(stop) {
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

/// This DLL's own module handle.
fn own_module() -> Result<HMODULE> {
    let mut module = ptr::null_mut();
    // SAFETY: Looks up the module containing this function, without changing its reference count.
    let found = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            own_module as *const u16,
            &mut module,
        )
    };
    if found == 0 { Err("cannot find the mod's own module".into()) } else { Ok(module) }
}

/// Returns the file path of `module`, or of the game's executable when `module` is null.
fn module_path(module: HMODULE) -> Result<PathBuf> {
    let mut buffer = vec![0u16; MAX_PATH as usize];
    loop {
        // SAFETY: The buffer is valid for `buffer.len()` UTF-16 units.
        let len = unsafe { GetModuleFileNameW(module, buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
        if len == 0 {
            return Err("GetModuleFileNameW failed".into());
        }
        if len < buffer.len() {
            buffer.truncate(len);
            return Ok(PathBuf::from(String::from_utf16_lossy(&buffer)));
        }
        buffer.resize(buffer.len() * 2, 0);
    }
}
