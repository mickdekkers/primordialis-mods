//! Where the mod's DLL and the game's executable are.

use std::ops::Range;
use std::path::PathBuf;
use std::ptr;

use windows_sys::Win32::Foundation::{HMODULE, MAX_PATH};
use windows_sys::Win32::System::Diagnostics::Debug::IMAGE_NT_HEADERS64;
use windows_sys::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    GetModuleFileNameW, GetModuleHandleExW, GetModuleHandleW,
};
use windows_sys::Win32::System::SystemServices::IMAGE_DOS_HEADER;

use crate::Result;

/// The mod DLL's module handle (this crate is linked into it).
pub fn own() -> Result<HMODULE> {
    let mut module = ptr::null_mut();
    // SAFETY: Looks up the module containing this function, without changing its reference count.
    let found = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            own as *const u16,
            &mut module,
        )
    };
    if found == 0 {
        Err("cannot find the mod's own module".into())
    } else {
        Ok(module)
    }
}

/// The address range of the mod DLL.
pub fn own_range() -> Result<Range<usize>> {
    let base = own()? as usize;
    // SAFETY: A loaded module starts with valid DOS and NT headers.
    let size = unsafe {
        let dos = &*(base as *const IMAGE_DOS_HEADER);
        let nt = &*((base + dos.e_lfanew as usize) as *const IMAGE_NT_HEADERS64);
        nt.OptionalHeader.SizeOfImage as usize
    };
    Ok(base..base + size)
}

/// Whether a module with this file name is loaded.
pub fn is_loaded(name: &str) -> bool {
    let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    // SAFETY: A NUL-terminated module name; doesn't load anything.
    !unsafe { GetModuleHandleW(name.as_ptr()) }.is_null()
}

/// Returns the file path of `module`, or of the game's executable when `module` is null.
pub fn path(module: HMODULE) -> Result<PathBuf> {
    let mut buffer = vec![0u16; MAX_PATH as usize];
    loop {
        // SAFETY: The buffer is valid for `buffer.len()` UTF-16 units.
        let len = unsafe { GetModuleFileNameW(module, buffer.as_mut_ptr(), buffer.len() as u32) }
            as usize;
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
