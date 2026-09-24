//! Development tool: loads the hot reload host into an already running Primordialis, for when the game
//! was started without it (`--customdll`). Run it from the directory with the host DLL:
//! `cargo run --release -p primordialis_qol_hot_reload --bin primordialis_qol_inject`.

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::process::ExitCode;
use std::ptr;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Diagnostics::Debug::WriteProcessMemory;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, MODULEENTRY32W, Module32FirstW, Module32NextW, PROCESSENTRY32W, Process32FirstW,
    Process32NextW, TH32CS_SNAPMODULE, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAllocEx, VirtualFreeEx,
};
use windows_sys::Win32::System::Threading::{
    CreateRemoteThread, GetExitCodeThread, OpenProcess, PROCESS_CREATE_THREAD, PROCESS_QUERY_INFORMATION,
    PROCESS_VM_OPERATION, PROCESS_VM_READ, PROCESS_VM_WRITE, WaitForSingleObject,
};

const HOST_DLL: &str = modkit_protocol::HOST_MODULE;
const MOD_DLL: &str = "primordialis_qol.dll";
/// The game's executables: the version selector starts one of the builds.
const GAME_EXES: &[&str] = &["primordialis.exe", "primordialis_avx.exe", "primordialis_sse3.exe"];

fn main() -> ExitCode {
    match inject() {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn inject() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find this program's path: {e}"))?;
    let host = exe.with_file_name(HOST_DLL);
    if !host.exists() {
        return Err(format!(
            "{} not found; build it with `cargo build --release -p primordialis_qol_hot_reload`",
            host.display()
        ));
    }
    let pid = find_game()?;
    let modules = module_names(pid)?;
    if modules.iter().any(|name| name.eq_ignore_ascii_case(HOST_DLL)) {
        return Ok(format!("the hot reload host is already loaded in process {pid}"));
    }
    if modules.iter().any(|name| name.eq_ignore_ascii_case(MOD_DLL)) {
        return Err(format!(
            "process {pid} already runs {MOD_DLL} directly (--customdll), which can't be swapped. Start the game \
             without it, or with --customdll pointing at {HOST_DLL}."
        ));
    }
    load_library(pid, &host)?;
    let log = host.with_file_name("primordialis_qol_hot_reload.log");
    Ok(format!("loaded {} into process {pid}; its log is {}", host.display(), log.display()))
}

fn find_game() -> Result<u32, String> {
    let mut found = Vec::new();
    // SAFETY: Toolhelp calls on a snapshot handle we own.
    unsafe {
        let snapshot = Snapshot::new(TH32CS_SNAPPROCESS, 0)?;
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snapshot.0, &mut entry) != 0;
        while more {
            let name = wide_to_string(&entry.szExeFile);
            if GAME_EXES.iter().any(|exe| name.eq_ignore_ascii_case(exe)) {
                found.push((entry.th32ProcessID, name));
            }
            more = Process32NextW(snapshot.0, &mut entry) != 0;
        }
    }
    match found.as_slice() {
        [] => Err("Primordialis isn't running".into()),
        [(pid, _)] => Ok(*pid),
        _ => Err(format!("several game processes are running: {found:?}")),
    }
}

fn module_names(pid: u32) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    // SAFETY: Toolhelp calls on a snapshot handle we own.
    unsafe {
        let snapshot = Snapshot::new(TH32CS_SNAPMODULE, pid)?;
        let mut entry: MODULEENTRY32W = std::mem::zeroed();
        entry.dwSize = size_of::<MODULEENTRY32W>() as u32;
        let mut more = Module32FirstW(snapshot.0, &mut entry) != 0;
        while more {
            names.push(wide_to_string(&entry.szModule));
            more = Module32NextW(snapshot.0, &mut entry) != 0;
        }
    }
    Ok(names)
}

/// Makes the game call `LoadLibraryW(path)` on a new thread, and waits for it.
fn load_library(pid: u32, path: &std::path::Path) -> Result<(), String> {
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let size = size_of_val(path.as_slice());
    // SAFETY: Standard remote-thread loading; every handle and allocation is released below.
    unsafe {
        let access = PROCESS_CREATE_THREAD
            | PROCESS_QUERY_INFORMATION
            | PROCESS_VM_OPERATION
            | PROCESS_VM_WRITE
            | PROCESS_VM_READ;
        let process = Handle(OpenProcess(access, 0, pid));
        if process.0.is_null() {
            return Err(format!("cannot open process {pid}: {}", std::io::Error::last_os_error()));
        }
        let remote = VirtualAllocEx(process.0, ptr::null(), size, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
        if remote.is_null() {
            return Err(format!("cannot allocate memory in the game: {}", std::io::Error::last_os_error()));
        }
        let result = call_load_library(process.0, remote, &path);
        // After a timeout, the game's thread may still read the path: leave it.
        if !matches!(result, Err(LoadError::TimedOut)) {
            VirtualFreeEx(process.0, remote, 0, MEM_RELEASE);
        }
        result.map_err(|error| match error {
            LoadError::TimedOut => "the game didn't finish loading the DLL within 30 seconds".into(),
            LoadError::Failed(message) => message,
        })
    }
}

enum LoadError {
    TimedOut,
    Failed(String),
}

/// # Safety
///
/// `remote` must be an allocation in `process` large enough for `path`.
unsafe fn call_load_library(process: HANDLE, remote: *mut c_void, path: &[u16]) -> Result<(), LoadError> {
    let failed = |what: &str| LoadError::Failed(format!("{what}: {}", std::io::Error::last_os_error()));
    // SAFETY: Guaranteed by the caller.
    unsafe {
        if WriteProcessMemory(process, remote, path.as_ptr().cast(), size_of_val(path), ptr::null_mut()) == 0 {
            return Err(failed("cannot write to the game's memory"));
        }
        // kernel32 is mapped at the same address in every process of a boot session.
        let kernel32 = GetModuleHandleW(wide("kernel32.dll").as_ptr());
        let load_library = GetProcAddress(kernel32, c"LoadLibraryW".as_ptr().cast())
            .ok_or_else(|| LoadError::Failed("cannot find LoadLibraryW".into()))?;
        let start: unsafe extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(load_library);
        let thread = Handle(CreateRemoteThread(process, ptr::null(), 0, Some(start), remote, 0, ptr::null_mut()));
        if thread.0.is_null() {
            return Err(failed("cannot start a thread in the game"));
        }
        if WaitForSingleObject(thread.0, 30_000) != WAIT_OBJECT_0 {
            return Err(LoadError::TimedOut);
        }
        // The low 32 bits of the loaded module's handle; 0 if loading failed.
        let mut exit_code = 0;
        GetExitCodeThread(thread.0, &mut exit_code);
        if exit_code == 0 { Err(LoadError::Failed("the game couldn't load the DLL".into())) } else { Ok(()) }
    }
}

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: A handle we own.
            unsafe { CloseHandle(self.0) };
        }
    }
}

struct Snapshot(HANDLE);

impl Snapshot {
    fn new(flags: u32, pid: u32) -> Result<Self, String> {
        // SAFETY: Creates a snapshot handle, closed on drop.
        let handle = unsafe { CreateToolhelp32Snapshot(flags, pid) };
        if handle == INVALID_HANDLE_VALUE {
            return Err(format!("cannot list processes or modules: {}", std::io::Error::last_os_error()));
        }
        Ok(Snapshot(handle))
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        // SAFETY: A handle we own.
        unsafe { CloseHandle(self.0) };
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn wide_to_string(wide: &[u16]) -> String {
    let len = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..len])
}
