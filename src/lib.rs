//! Primordialis QoL mod: shows the icon of every cell pickup on the map, for areas you have explored.
//!
//! Loaded by the game itself through its `--customdll "primordialis_qol.dll"` launch option. On load we
//! look up the game's internals by name in the debug symbols the game ships (`pdbs.zip`), hook two
//! rendering functions, and draw the icons with the game's own icon renderer while the map is open.
//! If anything doesn't match what we expect, we log why and leave the game unmodified.

mod game;
mod hooks;
mod log;
mod overlay;
mod symbols;

use std::ffi::c_void;
use std::panic;
use std::path::PathBuf;

use windows_sys::Win32::Foundation::{HMODULE, MAX_PATH, TRUE};
use windows_sys::Win32::System::LibraryLoader::{DisableThreadLibraryCalls, GetModuleFileNameW};
use windows_sys::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows_sys::core::BOOL;

pub type Result<T> = std::result::Result<T, String>;

#[unsafe(no_mangle)]
// The signature is dictated by Windows; the loader always passes our own, valid module handle.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "system" fn DllMain(module: HMODULE, reason: u32, _reserved: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        // SAFETY: `module` is our own module handle, provided by the loader.
        unsafe { DisableThreadLibraryCalls(module) };
        // The game loads us on its main thread while parsing the command line, before it creates any
        // other threads or renders anything. Doing all setup here means the hooks are in place before
        // the patched functions can ever run, so patching can't race with the game executing them.
        let result = panic::catch_unwind(|| init(module));
        match result {
            Ok(Ok(())) => log::info("ready: map icons enabled"),
            Ok(Err(error)) => log::error(&format!("not active, the game runs unmodified: {error}")),
            Err(_) => log::error("not active, the game runs unmodified: panicked during setup"),
        }
    }
    // Never fail the load: a broken mod should degrade to an unmodified game, not a crash.
    TRUE
}

fn init(module: HMODULE) -> Result<()> {
    let dll_path = module_path(module)?;
    let dll_dir = dll_path.parent().ok_or("DLL path has no parent directory")?.to_path_buf();
    log::init(&dll_dir.join("primordialis_qol.log"));
    log::info(&format!("primordialis_qol {} loaded from {}", env!("CARGO_PKG_VERSION"), dll_path.display()));

    let game_exe = module_path(std::ptr::null_mut())?;
    let game_dir = game_exe.parent().ok_or("game path has no parent directory")?;
    let symbols = symbols::load(game_dir, &dll_dir.join("primordialis_qol_cache"))?;
    let layout = game::Layout::resolve(&symbols)?;
    drop(symbols);

    hooks::install(layout)
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
