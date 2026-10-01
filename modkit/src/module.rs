//! Where the mod's DLL and the game's executable are.

use std::ffi::CStr;
use std::ops::Range;
use std::path::PathBuf;
use std::ptr;

use windows_sys::Win32::Foundation::{HMODULE, MAX_PATH};
use windows_sys::Win32::System::Diagnostics::Debug::IMAGE_NT_HEADERS64;
use windows_sys::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    GetModuleFileNameW, GetModuleHandleExW, GetModuleHandleW, GetProcAddress,
};
use windows_sys::Win32::System::SystemServices::IMAGE_DOS_HEADER;

use crate::Result;

/// The mod DLL's module handle (this crate is linked into it).
pub fn own() -> Result<HMODULE> {
    containing(own as *const () as usize).ok_or_else(|| "cannot find the mod's own module".into())
}

/// The loaded module whose image contains `address`, if any.
pub fn containing(address: usize) -> Option<HMODULE> {
    let mut module = ptr::null_mut();
    // SAFETY: Only looks the address up, without changing the module's reference count.
    let found = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            address as *const u16,
            &mut module,
        )
    };
    (found != 0).then_some(module)
}

/// Whether `module` exports a function or variable named `name`.
pub fn exports(module: HMODULE, name: &CStr) -> bool {
    // SAFETY: A loaded module and a NUL-terminated name; only looks the export up.
    unsafe { GetProcAddress(module, name.as_ptr().cast()) }.is_some()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_modules_and_their_exports() {
        let name: Vec<u16> = "kernel32.dll".encode_utf16().chain(Some(0)).collect();
        // SAFETY: A NUL-terminated module name; kernel32 is always loaded.
        let kernel32 = unsafe { GetModuleHandleW(name.as_ptr()) };
        assert!(!kernel32.is_null());
        // SAFETY: A loaded module and a NUL-terminated name.
        let function = unsafe { GetProcAddress(kernel32, c"GetProcAddress".as_ptr().cast()) };
        assert_eq!(containing(function.unwrap() as usize), Some(kernel32));
        assert!(exports(kernel32, c"GetProcAddress"));
        assert!(!exports(kernel32, c"modkit_api_version"));

        let heap = Box::new(0u8);
        assert_eq!(containing(&*heap as *const u8 as usize), None);
    }

    /// Here, this crate is linked into the test executable.
    #[test]
    fn finds_its_own_module() {
        let own = own().unwrap();
        // SAFETY: Only looks the executable's module handle up.
        assert_eq!(own, unsafe { GetModuleHandleW(ptr::null()) });
        let range = own_range().unwrap();
        assert_eq!(range.start, own as usize);
        let function = finds_its_own_module as *const () as usize;
        assert!(range.contains(&function), "{range:x?}");
        assert_eq!(containing(range.end - 1), Some(own));
        assert_eq!(path(own).unwrap(), std::env::current_exe().unwrap());
    }

    #[test]
    fn tells_whether_a_module_is_loaded() {
        assert!(is_loaded("kernel32.dll"));
        assert!(!is_loaded("no_such_module_here.dll"));
    }
}
