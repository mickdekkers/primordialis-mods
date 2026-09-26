//! Gives the DLL a Windows version resource, so that its properties in Explorer (Details) say what it
//! is: its name, version, description and copyright. Players checking a file they downloaded look
//! there, and antivirus heuristics distrust DLLs that say nothing about themselves.

use std::env;
use std::fs;
use std::path::PathBuf;

/// Shown as the product name. The same as the mod's title in `src/lib.rs`.
const PRODUCT_NAME: &str = "Primordialis QoL";

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=LICENSE");

    let package = |key: &str| env::var(format!("CARGO_PKG_{key}")).unwrap();
    let number = |part: &str| -> u16 { package(&format!("VERSION_{part}")).parse().unwrap() };
    let (major, minor, patch) = (number("MAJOR"), number("MINOR"), number("PATCH"));
    let name = package("NAME");
    // The license's copyright line, e.g. "Copyright (c) 2026 Mick Dekkers".
    let license = fs::read_to_string("LICENSE").unwrap();
    let copyright = license
        .lines()
        .find(|line| line.starts_with("Copyright"))
        .expect("LICENSE has no copyright line");

    let strings = [
        ("ProductName", PRODUCT_NAME.to_owned()),
        ("ProductVersion", package("VERSION")),
        ("FileDescription", package("DESCRIPTION")),
        ("FileVersion", package("VERSION")),
        ("InternalName", name.clone()),
        ("OriginalFilename", format!("{name}.dll")),
        (
            "LegalCopyright",
            format!("{copyright}. {} License.", package("LICENSE")),
        ),
        ("Comments", package("REPOSITORY")),
    ];
    let values: String = strings
        .iter()
        .map(|(key, value)| format!("            VALUE \"{key}\", \"{}\"\n", escape(value)))
        .collect();

    // The numbers are the Windows SDK's constants, written out so the file needs no #include.
    let rc = format!(
        "\
#pragma code_page(65001)
1 VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
FILEFLAGSMASK 0x3F
FILEFLAGS 0x0
FILEOS 0x40004 // VOS_NT_WINDOWS32
FILETYPE 0x2 // VFT_DLL
FILESUBTYPE 0x0
BEGIN
    BLOCK \"StringFileInfo\"
    BEGIN
        BLOCK \"040904B0\"
        BEGIN
{values}        END
    END
    BLOCK \"VarFileInfo\"
    BEGIN
        VALUE \"Translation\", 0x409, 1200
    END
END
"
    );
    let path = PathBuf::from(env::var("OUT_DIR").unwrap()).join("version.rc");
    fs::write(&path, rc).unwrap();
    // Required, not optional: a release must never go out without it.
    embed_resource::compile_for_cdylib(&path, embed_resource::NONE)
        .manifest_required()
        .unwrap();
}

/// Escapes a value for a string literal in a resource script.
fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\"\"")
}
