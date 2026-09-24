//! Looks up the game's functions, globals and struct layouts by name, using the debug symbols (PDB)
//! the game ships in `pdbs.zip`. The PDB matching the running executable is extracted into a cache
//! directory once, then queried through Windows' DbgHelp.

use std::collections::HashMap;
use std::ffi::c_void;
use std::fs::{self, File};
use std::io::{self, BufReader};
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::ptr;
use std::time::Instant;

use windows_sys::Win32::Foundation::{HANDLE, LocalFree};
use windows_sys::Win32::System::Diagnostics::Debug::{
    IMAGE_DEBUG_DIRECTORY, IMAGE_DEBUG_TYPE_CODEVIEW, IMAGE_DIRECTORY_ENTRY_DEBUG,
    IMAGE_NT_HEADERS64, IMAGEHLP_MODULEW64, IMAGEHLP_SYMBOL_TYPE_INFO, SYMBOL_INFOW,
    SYMOPT_FAIL_CRITICAL_ERRORS, SYMOPT_NO_PROMPTS, SYMOPT_UNDNAME, SymCleanup, SymEnumSymbolsW,
    SymFromNameW, SymGetModuleInfoW64, SymGetTypeFromNameW, SymGetTypeInfo, SymInitializeW,
    SymLoadModuleExW, SymPdb, SymSetOptions, SymSetScopeFromAddr, TI_FINDCHILDREN,
    TI_GET_BITPOSITION, TI_GET_CHILDRENCOUNT, TI_GET_LENGTH, TI_GET_OFFSET, TI_GET_SYMNAME,
    TI_GET_SYMTAG, TI_GET_TYPEID,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::SystemServices::IMAGE_DOS_HEADER;
use windows_sys::core::BOOL;

use crate::{Result, log};

/// DbgHelp identifies sessions by a caller-chosen handle, which "need not be a process handle".
/// Using our own value keeps our session separate from the game's crash handler, which also uses
/// DbgHelp (with the real process handle).
const SESSION: HANDLE = 0x5051_4F4C as HANDLE;

/// DIA symbol tags (`SymTagEnum`) we care about.
const SYM_TAG_FUNCTION: u32 = 5;
/// Data: global variables and struct members.
const SYM_TAG_DATA: u32 = 7;
/// User-defined type: a struct, class or union.
const SYM_TAG_UDT: u32 = 11;
const SYM_TAG_TYPEDEF: u32 = 17;

/// The executable's CodeView record, which identifies the exact PDB it was built with.
struct CodeView {
    guid: [u8; 16],
    age: u32,
    pdb_name: String,
}

impl CodeView {
    fn id(&self) -> String {
        let mut id: String = self.guid.iter().map(|b| format!("{b:02X}")).collect();
        id.push_str(&format!("{:X}", self.age));
        id
    }
}

/// An open DbgHelp session with the game's PDB loaded at the executable's actual base address, so
/// every address it returns is directly usable.
pub struct Symbols {
    base: u64,
}

#[derive(Clone, Copy)]
struct Found {
    address: u64,
    type_id: u32,
}

pub struct Field {
    pub offset: usize,
    /// The size of the field's type, in bytes.
    pub size: usize,
    /// For bitfields: the position of the lowest bit, and the number of bits.
    pub bits: Option<(u32, u64)>,
}

pub struct TypeLayout {
    pub name: String,
    pub size: usize,
    fields: HashMap<String, Field>,
}

impl TypeLayout {
    pub fn offset(&self, field: &str) -> Result<usize> {
        let info = self.field(field)?;
        if info.bits.is_some() {
            return Err(format!("{}.{field} is unexpectedly a bitfield", self.name));
        }
        Ok(info.offset)
    }

    /// Byte offset of a field that is `size` bytes large. Fails if its size changed, which means its
    /// type did: reading or writing it as before would reach into the fields next to it.
    pub fn offset_sized(&self, field: &str, size: usize) -> Result<usize> {
        let offset = self.offset(field)?;
        let actual = self.field(field)?.size;
        if actual != size {
            return Err(format!(
                "{}.{field} is {actual} bytes, expected {size}",
                self.name
            ));
        }
        Ok(offset)
    }

    /// Byte offset of a field read or written as a `T`, checking that it's as large as one.
    pub fn offset_of<T>(&self, field: &str) -> Result<usize> {
        self.offset_sized(field, size_of::<T>())
    }

    /// Byte offset and bit position of a one-bit bitfield.
    pub fn flag(&self, field: &str) -> Result<(usize, u32)> {
        let info = self.field(field)?;
        match info.bits {
            Some((position, 1)) => Ok((info.offset, position)),
            _ => Err(format!("{}.{field} is no longer a one-bit flag", self.name)),
        }
    }

    fn field(&self, field: &str) -> Result<&Field> {
        self.fields
            .get(field)
            .ok_or_else(|| format!("struct {} has no field `{field}`", self.name))
    }
}

/// Finds and loads the PDB matching the running game executable.
pub fn load(game_dir: &Path, cache_dir: &Path) -> Result<Symbols> {
    // SAFETY: A null name returns the handle of the executable, which stays loaded for the process
    // lifetime and is a mapped PE image.
    unsafe { load_for_image(GetModuleHandleW(ptr::null()) as usize, game_dir, cache_dir) }
}

/// Finds and loads the PDB matching the executable image mapped at `base`.
///
/// # Safety
///
/// `base` must point to a PE image mapped by the Windows loader, which stays mapped while the
/// returned `Symbols` is used.
pub unsafe fn load_for_image(base: usize, game_dir: &Path, cache_dir: &Path) -> Result<Symbols> {
    let started = Instant::now();
    // SAFETY: Forwarded from the caller.
    let (codeview, image_size) = unsafe { read_codeview(base)? };
    log::info(&format!(
        "game executable expects {} ({})",
        codeview.pdb_name,
        codeview.id()
    ));

    let pdb_path = extract_pdb(game_dir, cache_dir, &codeview)?;
    let symbols = Symbols::open(&pdb_path, base, image_size, &codeview)?;
    log::info(&format!(
        "loaded symbols from {} in {:.0?}",
        pdb_path.display(),
        started.elapsed()
    ));
    Ok(symbols)
}

/// Reads the CodeView (RSDS) debug record and the image size from a mapped PE image.
///
/// # Safety
///
/// `base` must point to a PE image mapped by the Windows loader.
unsafe fn read_codeview(base: usize) -> Result<(CodeView, u32)> {
    // SAFETY: `base` is a mapped PE image, as guaranteed by the caller, so its headers, its debug
    // directory and the records that points to are mapped and readable.
    unsafe {
        let dos = &*(base as *const IMAGE_DOS_HEADER);
        if dos.e_magic != 0x5A4D {
            return Err("game executable has no MZ header".into());
        }
        let nt = &*((base + dos.e_lfanew as usize) as *const IMAGE_NT_HEADERS64);
        if nt.Signature != 0x0000_4550 {
            return Err("game executable has no PE header".into());
        }
        let image_size = nt.OptionalHeader.SizeOfImage;
        let directory = nt.OptionalHeader.DataDirectory[IMAGE_DIRECTORY_ENTRY_DEBUG as usize];
        let count = directory.Size as usize / size_of::<IMAGE_DEBUG_DIRECTORY>();
        let entries = (base + directory.VirtualAddress as usize) as *const IMAGE_DEBUG_DIRECTORY;
        for i in 0..count {
            let entry = &*entries.add(i);
            if entry.Type != IMAGE_DEBUG_TYPE_CODEVIEW || entry.AddressOfRawData == 0 {
                continue;
            }
            let record = (base + entry.AddressOfRawData as usize) as *const u8;
            if std::slice::from_raw_parts(record, 4) != b"RSDS" {
                continue;
            }
            let mut guid = [0u8; 16];
            guid.copy_from_slice(std::slice::from_raw_parts(record.add(4), 16));
            let age = ptr::read_unaligned(record.add(20) as *const u32);
            let path = std::ffi::CStr::from_ptr(record.add(24).cast())
                .to_string_lossy()
                .into_owned();
            // The record holds the build machine's full path; the zip only has the file name.
            let pdb_name = path.rsplit(['\\', '/']).next().unwrap_or(&path).to_owned();
            return Ok((
                CodeView {
                    guid,
                    age,
                    pdb_name,
                },
                image_size,
            ));
        }
        Err("game executable has no CodeView debug record".into())
    }
}

/// Extracts the PDB named in the CodeView record from the game's `pdbs.zip`, unless a previous run
/// already did. The cache is keyed by the PDB's GUID, so a game update gets a fresh extraction.
fn extract_pdb(game_dir: &Path, cache_dir: &Path, codeview: &CodeView) -> Result<PathBuf> {
    let id = codeview.id();
    let dir = cache_dir.join(&id);
    let pdb_path = dir.join(&codeview.pdb_name);
    if pdb_path.is_file() {
        return Ok(pdb_path);
    }

    let zip_path = game_dir.join("pdbs.zip");
    let zip_file =
        File::open(&zip_path).map_err(|e| format!("cannot open {}: {e}", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(BufReader::new(zip_file))
        .map_err(|e| format!("cannot read {}: {e}", zip_path.display()))?;
    let entry_name = archive
        .file_names()
        .find(|name| name.eq_ignore_ascii_case(&codeview.pdb_name))
        .map(str::to_owned)
        .ok_or_else(|| {
            format!(
                "{} does not contain {}",
                zip_path.display(),
                codeview.pdb_name
            )
        })?;
    let mut entry = archive
        .by_name(&entry_name)
        .map_err(|e| format!("cannot read {entry_name}: {e}"))?;

    fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    // Extract under a temporary name so an interrupted extraction is never mistaken for a complete one.
    let partial = dir.join(format!("{}.partial", codeview.pdb_name));
    let copied = File::create(&partial).and_then(|mut out| io::copy(&mut entry, &mut out));
    if let Err(error) = copied.and_then(|_| fs::rename(&partial, &pdb_path)) {
        let _ = fs::remove_file(&partial);
        return Err(format!(
            "cannot extract {entry_name} to {}: {error}",
            dir.display()
        ));
    }
    log::info(&format!("extracted {entry_name} from pdbs.zip"));
    remove_stale_cache_entries(cache_dir, &id);
    Ok(pdb_path)
}

/// Deletes PDBs extracted for previous game versions. Only touches directories whose names look like
/// our cache keys (GUID + age in hex), so nothing else can be deleted by accident.
fn remove_stale_cache_entries(cache_dir: &Path, current_id: &str) {
    let Ok(entries) = fs::read_dir(cache_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let looks_like_key = name.len() > 32 && name.chars().all(|c| c.is_ascii_hexdigit());
        if looks_like_key && name != current_id && entry.path().is_dir() {
            match fs::remove_dir_all(entry.path()) {
                Ok(()) => log::info(&format!(
                    "removed symbols cached for an older game version ({name})"
                )),
                Err(e) => log::warn(&format!(
                    "could not remove old cache {}: {e}",
                    entry.path().display()
                )),
            }
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// A `SYMBOL_INFOW` followed by room for the symbol name, as DbgHelp expects.
#[repr(C)]
struct SymbolInfo {
    info: SYMBOL_INFOW,
    name: [u16; 256],
}

impl SymbolInfo {
    fn new() -> Box<Self> {
        // SAFETY: SYMBOL_INFOW and the name buffer are plain data, for which all-zero is valid.
        let mut symbol: Box<Self> = Box::new(unsafe { std::mem::zeroed() });
        symbol.info.SizeOfStruct = size_of::<SYMBOL_INFOW>() as u32;
        symbol.info.MaxNameLen = 256;
        symbol
    }
}

impl Symbols {
    fn open(pdb_path: &Path, base: usize, image_size: u32, codeview: &CodeView) -> Result<Self> {
        // SAFETY: Plain DbgHelp calls with valid, NUL-terminated arguments. The session is closed in Drop.
        unsafe {
            SymSetOptions(SYMOPT_UNDNAME | SYMOPT_FAIL_CRITICAL_ERRORS | SYMOPT_NO_PROMPTS);
            if SymInitializeW(SESSION, ptr::null(), 0) == 0 {
                return Err("SymInitializeW failed".into());
            }
            let symbols = Symbols { base: base as u64 };
            let path = wide(&pdb_path.to_string_lossy());
            let loaded = SymLoadModuleExW(
                SESSION,
                ptr::null_mut(),
                path.as_ptr(),
                ptr::null(),
                base as u64,
                image_size,
                ptr::null(),
                0,
            );
            if loaded == 0 {
                return Err(format!("DbgHelp could not load {}", pdb_path.display()));
            }

            let mut module: IMAGEHLP_MODULEW64 = std::mem::zeroed();
            module.SizeOfStruct = size_of::<IMAGEHLP_MODULEW64>() as u32;
            if SymGetModuleInfoW64(SESSION, base as u64, &mut module) == 0 {
                return Err("SymGetModuleInfoW64 failed".into());
            }
            let signature: [u8; 16] = std::mem::transmute(module.PdbSig70);
            if module.SymType != SymPdb
                || signature != codeview.guid
                || module.PdbAge != codeview.age
            {
                return Err(format!(
                    "{} does not match the running game executable (expected {})",
                    pdb_path.display(),
                    codeview.id()
                ));
            }
            Ok(symbols)
        }
    }

    /// Address of a global function or variable.
    pub fn address(&self, name: &str) -> Result<usize> {
        Ok(self.find_global(name)?.address as usize)
    }

    /// Address of the global function `name` that takes `params` parameters. C++ overloads share a
    /// name, and `address` would pick one of them arbitrarily; the parameter count tells them apart
    /// (and checks that a function without overloads still has the signature we call it with).
    pub fn function(&self, name: &str, params: usize) -> Result<usize> {
        let mut found = self
            .enumerate(self.base, name, Some(SYM_TAG_FUNCTION))
            .ok_or_else(|| format!("cannot enumerate the functions named `{name}`"))?;
        found.sort_by_key(|found| found.address);
        found.dedup_by_key(|found| found.address);
        let mut matching = Vec::new();
        for found in &found {
            // A function's type is its signature, whose children are its parameters.
            let mut count = 0u32;
            self.type_info(found.type_id, TI_GET_CHILDRENCOUNT, &mut count)
                .ok_or_else(|| format!("cannot get the parameters of `{name}`"))?;
            if count as usize == params {
                matching.push(*found);
            }
        }
        match matching[..] {
            [found] => {
                self.check_address(name, found.address)?;
                Ok(found.address as usize)
            }
            [] if found.is_empty() => Err(format!("function `{name}` not found")),
            [] => Err(format!(
                "no function `{name}` takes {params} parameters anymore"
            )),
            _ => Err(format!(
                "several functions `{name}` take {params} parameters"
            )),
        }
    }

    /// Size of a global variable, in bytes.
    pub fn variable_size(&self, name: &str) -> Result<usize> {
        self.type_length(self.find_global(name)?.type_id)
    }

    /// Address and size of a `static` variable declared inside `function`.
    pub fn function_static(&self, function: &str, name: &str) -> Result<(usize, usize)> {
        let function_address = self.find_global(function)?.address;
        // SAFETY: A plain DbgHelp call on our session.
        let scoped = unsafe { SymSetScopeFromAddr(SESSION, function_address) } != 0;
        // A zero module base enumerates the symbols of the scope set just before.
        let mut found = scoped
            .then(|| self.enumerate(0, name, None))
            .flatten()
            .ok_or_else(|| format!("cannot enumerate the variables of `{function}`"))?;
        found.dedup_by_key(|found| found.address);
        let found = match found[..] {
            [found] => found,
            [] => return Err(format!("`{function}` has no static variable `{name}`")),
            _ => {
                return Err(format!(
                    "`{function}` has several static variables named `{name}`"
                ));
            }
        };
        self.check_address(name, found.address)?;
        Ok((found.address as usize, self.type_length(found.type_id)?))
    }

    /// The symbols named `name` that have an address (locals don't; globals and statics do), and the
    /// symbol tag `tag` if one is given. Searches the module at base address `module`, or with 0, the
    /// scope set with `SymSetScopeFromAddr`. `None` if DbgHelp can't enumerate them.
    fn enumerate(&self, module: u64, name: &str, tag: Option<u32>) -> Option<Vec<Found>> {
        struct Search<'a> {
            name: &'a str,
            tag: Option<u32>,
            found: Vec<Found>,
        }
        unsafe extern "system" fn collect(
            info: *const SYMBOL_INFOW,
            _size: u32,
            context: *const c_void,
        ) -> BOOL {
            // SAFETY: DbgHelp passes a valid symbol whose name is `NameLen` characters long, and the
            // context is the `Search` passed to `SymEnumSymbolsW` below, which outlives the enumeration.
            unsafe {
                let info = &*info;
                let search = &mut *(context as *mut Search);
                let symbol_name =
                    std::slice::from_raw_parts(info.Name.as_ptr(), info.NameLen as usize);
                let symbol_name = String::from_utf16_lossy(symbol_name);
                if symbol_name.trim_end_matches('\0') == search.name
                    && info.Address != 0
                    && search.tag.is_none_or(|tag| info.Tag == tag)
                {
                    search.found.push(Found {
                        address: info.Address,
                        type_id: info.TypeIndex,
                    });
                }
            }
            1
        }

        let mut search = Search {
            name,
            tag,
            found: Vec::new(),
        };
        let wide_name = wide(name);
        // SAFETY: The callback matches DbgHelp's signature and only uses the context we pass here.
        let ok = unsafe {
            SymEnumSymbolsW(
                SESSION,
                module,
                wide_name.as_ptr(),
                Some(collect),
                &mut search as *mut Search as *const c_void,
            ) != 0
        };
        ok.then_some(search.found)
    }

    fn find_global(&self, name: &str) -> Result<Found> {
        let mut symbol = SymbolInfo::new();
        let wide_name = wide(name);
        // SAFETY: `symbol` is a correctly sized SYMBOL_INFOW with room for the name.
        if unsafe { SymFromNameW(SESSION, wide_name.as_ptr(), &mut symbol.info) } == 0 {
            return Err(format!("symbol `{name}` not found"));
        }
        let info = &symbol.info;
        if info.Tag != SYM_TAG_FUNCTION && info.Tag != SYM_TAG_DATA {
            return Err(format!(
                "symbol `{name}` is not a function or variable (tag {})",
                info.Tag
            ));
        }
        self.check_address(name, info.Address)?;
        Ok(Found {
            address: info.Address,
            type_id: info.TypeIndex,
        })
    }

    fn check_address(&self, name: &str, address: u64) -> Result<()> {
        if address < self.base {
            return Err(format!(
                "symbol `{name}` has an unexpected address {address:#x}"
            ));
        }
        Ok(())
    }

    /// Size and data members of a struct.
    pub fn layout(&self, type_name: &str) -> Result<TypeLayout> {
        let mut symbol = SymbolInfo::new();
        let wide_name = wide(type_name);
        // SAFETY: As in `address`.
        if unsafe { SymGetTypeFromNameW(SESSION, self.base, wide_name.as_ptr(), &mut symbol.info) }
            == 0
        {
            return Err(format!("type `{type_name}` not found"));
        }
        let type_id = self.resolve_typedefs(symbol.info.TypeIndex, type_name)?;
        let size = self.type_length(type_id)?;

        let mut count = 0u32;
        self.type_info(type_id, TI_GET_CHILDRENCOUNT, &mut count)
            .ok_or_else(|| format!("cannot enumerate members of `{type_name}`"))?;
        // TI_FINDCHILDREN_PARAMS is { Count, Start, ChildId[Count] }.
        let mut children = vec![0u32; 2 + count as usize];
        children[0] = count;
        self.type_info(type_id, TI_FINDCHILDREN, children.as_mut_ptr())
            .ok_or_else(|| format!("cannot enumerate members of `{type_name}`"))?;

        let mut fields = HashMap::new();
        for &child in &children[2..] {
            let mut tag = 0u32;
            if self.type_info(child, TI_GET_SYMTAG, &mut tag).is_none() || tag != SYM_TAG_DATA {
                continue;
            }
            let mut offset = 0u32;
            let Some(name) = self.type_name(child) else {
                continue;
            };
            if self.type_info(child, TI_GET_OFFSET, &mut offset).is_none() {
                continue;
            }
            let mut position = 0u32;
            let bits = match self.type_info(child, TI_GET_BITPOSITION, &mut position) {
                // A bitfield member's length is its number of bits.
                Some(()) => Some((position, self.type_length(child)? as u64)),
                None => None,
            };
            let mut field_type = 0u32;
            let size = match self.type_info(child, TI_GET_TYPEID, &mut field_type) {
                Some(()) => self.type_length(field_type)?,
                // Unknown: no size check can pass.
                None => 0,
            };
            fields.insert(
                name,
                Field {
                    offset: offset as usize,
                    size,
                    bits,
                },
            );
        }
        Ok(TypeLayout {
            name: type_name.to_owned(),
            size,
            fields,
        })
    }

    /// C code declares most structs as `typedef struct name {...} name;`, so a name can resolve to a
    /// typedef. Follows typedefs to the struct (user-defined type) they name.
    fn resolve_typedefs(&self, mut type_id: u32, type_name: &str) -> Result<u32> {
        for _ in 0..8 {
            let mut tag = 0u32;
            self.type_info(type_id, TI_GET_SYMTAG, &mut tag)
                .ok_or_else(|| format!("cannot get the kind of type `{type_name}`"))?;
            match tag {
                SYM_TAG_UDT => return Ok(type_id),
                SYM_TAG_TYPEDEF => {
                    self.type_info(type_id, TI_GET_TYPEID, &mut type_id)
                        .ok_or_else(|| format!("cannot follow typedef `{type_name}`"))?;
                }
                _ => return Err(format!("`{type_name}` is not a struct (symbol tag {tag})")),
            }
        }
        Err(format!("typedef chain for `{type_name}` is too deep"))
    }

    fn type_length(&self, type_id: u32) -> Result<usize> {
        let mut length = 0u64;
        self.type_info(type_id, TI_GET_LENGTH, &mut length)
            .ok_or("cannot get type size")?;
        Ok(length as usize)
    }

    fn type_name(&self, type_id: u32) -> Option<String> {
        let mut name: *mut u16 = ptr::null_mut();
        self.type_info(type_id, TI_GET_SYMNAME, &mut name)?;
        if name.is_null() {
            return None;
        }
        // SAFETY: DbgHelp returns a NUL-terminated string allocated with LocalAlloc, which we free.
        unsafe {
            let len = (0..).take_while(|&i| *name.add(i) != 0).count();
            let result = String::from_utf16_lossy(std::slice::from_raw_parts(name, len));
            LocalFree(name.cast());
            Some(result)
        }
    }

    fn type_info<T>(
        &self,
        type_id: u32,
        request: IMAGEHLP_SYMBOL_TYPE_INFO,
        out: *mut T,
    ) -> Option<()> {
        // SAFETY: Every call site passes an output buffer of the type DbgHelp documents for `request`.
        let ok =
            unsafe { SymGetTypeInfo(SESSION, self.base, type_id, request, out as *mut c_void) };
        (ok != 0).then_some(())
    }
}

impl Drop for Symbols {
    fn drop(&mut self) {
        // SAFETY: Closes the session opened in `open`.
        unsafe { SymCleanup(SESSION) };
    }
}
