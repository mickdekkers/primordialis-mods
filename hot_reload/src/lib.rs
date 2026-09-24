//! Development tool: loads `primordialis_qol.dll` into the running game, and swaps in each new build
//! of it without restarting the game.
//!
//! Load this DLL instead of the mod (`--customdll "...\primordialis_qol_hot_reload.dll"`, or inject it
//! into a running game with `primordialis_qol_inject`). It expects `primordialis_qol.dll` in its own
//! directory, and loads a copy of it, so that the original stays free for the build to overwrite.
//! When the original changes and the build has finished writing it:
//!
//! 1. The new build is copied (with its PDB) and loaded, its entry points are checked, and it
//!    prepares (loads its settings and symbols) while the running build keeps working. If any of that
//!    fails, the running build stays.
//! 2. The running copy is stopped: it removes its hooks once no thread is inside it, restores
//!    everything it changed, and frees its memory. If it can't do that safely, it keeps running and
//!    the new build is discarded.
//! 3. The old copy is unloaded, and the new one hooks the game, a few milliseconds after the old one
//!    let go. If it fails to, the previous build is started again.
//!
//! The mod writes its log, settings and symbol cache next to this DLL; this host logs to
//! `primordialis_qol_hot_reload.log`.

use std::ffi::c_void;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Foundation::{FreeLibrary, HMODULE, MAX_PATH, TRUE};
use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
use windows_sys::Win32::System::LibraryLoader::{
    DisableThreadLibraryCalls, GetModuleFileNameW, GetModuleHandleW, GetProcAddress, LoadLibraryW,
};
use windows_sys::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows_sys::Win32::System::Threading::GetCurrentProcessId;
use windows_sys::core::BOOL;

/// The mod's file name, without extension.
const MOD_NAME: &str = "primordialis_qol";
/// Where copies of the mod are loaded from, inside this DLL's directory.
const COPIES_DIR: &str = "hot_reload";
const LOG_FILE: &str = "primordialis_qol_hot_reload.log";
/// The version of the mod's host API this host speaks (`primordialis_qol_host_api_version`).
const HOST_API_VERSION: u32 = 2;

/// How often to check the mod for changes.
const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// A new build is only loaded once its file hasn't changed for this long.
const SETTLE_TIME: Duration = Duration::from_millis(500);

type ApiVersion = unsafe extern "C" fn() -> u32;
type Prepare = unsafe extern "C" fn(*const u16, usize) -> bool;
type Start = unsafe extern "C" fn() -> bool;
type Stop = unsafe extern "C" fn() -> bool;

#[unsafe(no_mangle)]
// The signature is dictated by Windows; the loader always passes our own, valid module handle.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "system" fn DllMain(module: HMODULE, reason: u32, _reserved: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        // SAFETY: `module` is our own module handle, provided by the loader.
        unsafe { DisableThreadLibraryCalls(module) };
        // Everything happens on our own thread, outside the loader lock. The host stays loaded until
        // the game exits.
        let module = module as usize;
        let _ = thread::Builder::new()
            .name("primordialis_qol hot reload".into())
            .spawn(move || run(module as HMODULE));
    }
    TRUE
}

fn run(module: HMODULE) {
    let Some(dir) = module_path(module).and_then(|path| path.parent().map(Path::to_path_buf)) else { return };
    let mut host = Host { log: File::create(dir.join(LOG_FILE)).ok(), generation: 0, running: None, dir };
    host.log(&format!("hot reload host running in process {}", std::process::id()));

    let standalone: Vec<u16> = format!("{MOD_NAME}.dll").encode_utf16().chain(Some(0)).collect();
    // SAFETY: A NUL-terminated module name; doesn't load anything.
    if !unsafe { GetModuleHandleW(standalone.as_ptr()) }.is_null() {
        host.log(&format!(
            "{MOD_NAME}.dll is already loaded directly (--customdll?), so it can't be swapped. Load only \
             this host instead. Not doing anything."
        ));
        return;
    }
    host.remove_old_copies();

    let source = host.dir.join(format!("{MOD_NAME}.dll"));
    let mut loaded_version = None;
    loop {
        let version = file_version(&source);
        if version.is_some() && version != loaded_version && is_settled(&source, version) {
            loaded_version = version;
            host.swap_in(&source);
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Changes whenever the file is rewritten.
fn file_version(path: &Path) -> Option<(SystemTime, u64)> {
    let metadata = fs::metadata(path).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

/// Whether the build has finished writing the file: it hasn't changed for a while, and nothing has
/// it open for writing.
fn is_settled(path: &Path, version: Option<(SystemTime, u64)>) -> bool {
    thread::sleep(SETTLE_TIME);
    file_version(path) == version && OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(path).is_ok()
}

/// A loaded copy of the mod.
struct Build {
    module: HMODULE,
    dir: PathBuf,
    dll: PathBuf,
    generation: u32,
    prepare: Prepare,
    start: Start,
    stop: Stop,
}

struct Host {
    log: Option<File>,
    dir: PathBuf,
    generation: u32,
    running: Option<Build>,
}

impl Host {
    fn log(&mut self, message: &str) {
        let Some(file) = self.log.as_mut() else { return };
        let seconds = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
        let _ = writeln!(file, "[{seconds:.3}] {message}");
        let _ = file.flush();
    }

    /// Copies of the mod from earlier runs of the game. Ones still in use (by another instance of
    /// the game) can't be deleted, and are left alone.
    fn remove_old_copies(&mut self) {
        let Ok(entries) = fs::read_dir(self.dir.join(COPIES_DIR)) else { return };
        for entry in entries.flatten() {
            let _ = fs::remove_dir_all(entry.path());
        }
    }

    fn swap_in(&mut self, source: &Path) {
        let started = Instant::now();
        self.generation += 1;
        let generation = self.generation;
        self.log(&format!("build {generation}: {} changed, loading it", source.display()));
        // Everything slow happens while the running build keeps working.
        let new = match self.load_copy(source, generation) {
            Ok(new) => new,
            Err(error) => {
                self.log(&format!("build {generation}: cannot load it, keeping the running build: {error}"));
                return;
            }
        };
        if !self.prepare(&new) {
            self.log(&format!(
                "build {generation}: cannot start it (see primordialis_qol.log), keeping the running build"
            ));
            unload(new);
            return;
        }

        // The mod isn't active from here until the new build's hooks are installed.
        let handover = Instant::now();
        let previous = self.running.take();
        if let Some(old) = &previous {
            // SAFETY: The copy's entry point, checked when loading it.
            if !unsafe { (old.stop)() } {
                self.log(&format!(
                    "build {generation}: build {} can't be stopped safely (see primordialis_qol.log), so it keeps \
                     running and the new build is discarded. Rebuild to try again.",
                    old.generation
                ));
                unload(new);
                self.running = previous;
                return;
            }
            // SAFETY: Stopped: its hooks are removed and none of its code runs anymore.
            unsafe { FreeLibrary(old.module) };
        }

        // SAFETY: The copy's entry point, checked when loading it; prepared above.
        if unsafe { (new.start)() } {
            let gap = handover.elapsed();
            self.log(&format!(
                "build {generation}: running (swapped in {:?}, mod inactive for {gap:?})",
                started.elapsed()
            ));
            self.running = Some(new);
            if let Some(old) = previous {
                let _ = fs::remove_dir_all(&old.dir);
            }
            return;
        }
        self.log(&format!("build {generation}: failed to start (see primordialis_qol.log)"));
        unload(new);
        let Some(old) = previous else { return };
        // The previous build's copy is still on disk: bring it back.
        match self.load_copy(&old.dll, old.generation) {
            // SAFETY: The copy's entry point, checked when loading it; prepared first.
            Ok(copy) if self.prepare(&copy) && unsafe { (copy.start)() } => {
                self.log(&format!("build {generation}: restarted build {} instead", old.generation));
                self.running = Some(copy);
            }
            Ok(copy) => {
                self.log(&format!("build {generation}: build {} failed to start again too", old.generation));
                unload(copy);
            }
            Err(error) => self.log(&format!("build {generation}: cannot reload build {}: {error}", old.generation)),
        }
    }

    fn prepare(&mut self, build: &Build) -> bool {
        let home: Vec<u16> = self.dir.as_os_str().encode_wide().collect();
        // SAFETY: The copy's entry point, checked when loading it, with a valid UTF-16 slice.
        unsafe { (build.prepare)(home.as_ptr(), home.len()) }
    }

    /// Copies the mod (and its PDB, for debuggers) to its own directory, and loads it. The copy's
    /// `DllMain` does nothing when this host is loaded; it's prepared and started separately.
    fn load_copy(&mut self, source: &Path, generation: u32) -> Result<Build, String> {
        // SAFETY: Always safe.
        let dir = self.dir.join(COPIES_DIR).join(format!("{}-{generation}", unsafe { GetCurrentProcessId() }));
        let dll = dir.join(format!("{MOD_NAME}_{generation}.dll"));
        if source != dll {
            fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
            fs::copy(source, &dll).map_err(|e| format!("cannot copy to {}: {e}", dll.display()))?;
            // Debuggers look for the PDB by its original name, next to the DLL.
            let pdb = source.with_file_name(format!("{MOD_NAME}.pdb"));
            if pdb.exists() {
                let _ = fs::copy(&pdb, dir.join(format!("{MOD_NAME}.pdb")));
            }
        }
        let wide: Vec<u16> = dll.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: A NUL-terminated path.
        let module = unsafe { LoadLibraryW(wide.as_ptr()) };
        if module.is_null() {
            let _ = fs::remove_dir_all(&dir);
            return Err(format!("LoadLibrary failed: {}", std::io::Error::last_os_error()));
        }
        // SAFETY: The version export has this signature in every build, and the others in every build
        // with this API version.
        let entry_points = unsafe {
            match export::<ApiVersion>(module, c"primordialis_qol_host_api_version").map(|version| version()) {
                Some(HOST_API_VERSION) => match (
                    export::<Prepare>(module, c"primordialis_qol_prepare"),
                    export::<Start>(module, c"primordialis_qol_start"),
                    export::<Stop>(module, c"primordialis_qol_stop"),
                ) {
                    (Some(prepare), Some(start), Some(stop)) => Ok((prepare, start, stop)),
                    _ => Err("it doesn't export all hot reload entry points".to_owned()),
                },
                Some(found) => Err(format!(
                    "it speaks host API version {found}, this host {HOST_API_VERSION}; rebuild the host (with the \
                     game closed)"
                )),
                None => Err("it doesn't export the hot reload entry points".to_owned()),
            }
        };
        match entry_points {
            Ok((prepare, start, stop)) => Ok(Build { module, dir, dll, generation, prepare, start, stop }),
            Err(error) => {
                // SAFETY: Never started.
                unsafe { FreeLibrary(module) };
                let _ = fs::remove_dir_all(&dir);
                Err(error)
            }
        }
    }
}

/// Unloads a copy that isn't running, and deletes it.
fn unload(copy: Build) {
    // SAFETY: Not started (or failed to start), so none of its code runs.
    unsafe { FreeLibrary(copy.module) };
    let _ = fs::remove_dir_all(&copy.dir);
}

/// # Safety
///
/// `T` must be the export's function pointer type.
unsafe fn export<T: Copy>(module: HMODULE, name: &std::ffi::CStr) -> Option<T> {
    // SAFETY: A NUL-terminated name.
    let address = unsafe { GetProcAddress(module, name.as_ptr().cast()) }?;
    // SAFETY: Guaranteed by the caller; function pointers are all the same size.
    Some(unsafe { std::mem::transmute_copy(&address) })
}

fn module_path(module: HMODULE) -> Option<PathBuf> {
    let mut buffer = vec![0u16; MAX_PATH as usize];
    loop {
        // SAFETY: The buffer is valid for `buffer.len()` UTF-16 units.
        let len = unsafe { GetModuleFileNameW(module, buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
        if len == 0 {
            return None;
        }
        if len < buffer.len() {
            buffer.truncate(len);
            return Some(PathBuf::from(String::from_utf16_lossy(&buffer)));
        }
        buffer.resize(buffer.len() * 2, 0);
    }
}
