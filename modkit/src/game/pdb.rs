//! Finding the PDB the running executable was built with: its CodeView record names it, and the
//! game's `pdbs.zip` holds it, extracted into a cache directory once.

use std::fs::{self, File};
use std::io::{self, BufReader};
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::ptr;

use windows_sys::Win32::System::Diagnostics::Debug::{
    IMAGE_DEBUG_DIRECTORY, IMAGE_DEBUG_TYPE_CODEVIEW, IMAGE_DIRECTORY_ENTRY_DEBUG,
    IMAGE_NT_HEADERS64,
};
use windows_sys::Win32::System::SystemServices::IMAGE_DOS_HEADER;

use crate::{Result, log};

/// The executable's CodeView record, which identifies the exact PDB it was built with.
pub struct CodeView {
    pub guid: [u8; 16],
    pub age: u32,
    pub pdb_name: String,
}

impl CodeView {
    pub fn id(&self) -> String {
        let mut id: String = self.guid.iter().map(|b| format!("{b:02X}")).collect();
        id.push_str(&format!("{:X}", self.age));
        id
    }
}

/// Reads the CodeView (RSDS) debug record and the image size from a mapped PE image.
///
/// # Safety
///
/// `base` must point to a PE image mapped by the Windows loader.
pub unsafe fn read_codeview(base: usize) -> Result<(CodeView, u32)> {
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
pub fn extract(game_dir: &Path, cache_dir: &Path, codeview: &CodeView) -> Result<PathBuf> {
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
