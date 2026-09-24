//! What a mod built with `modkit` and the hot reload host agree on: the functions the mod exports
//! for the host, and their version. Shared by both, so they can't drift apart.
//!
//! The host loads a copy of the mod, checks `API_VERSION` through `EXPORT_API_VERSION`, then calls
//! `EXPORT_PREPARE` (while the previous copy still runs), `EXPORT_START` once the previous copy is
//! stopped, and `EXPORT_STOP` before unloading it. `modkit::entry!` defines these exports.

use std::ffi::CStr;

/// Version of the functions below. Bump it on any incompatible change to them: a host then refuses
/// mods with a different version, and keeps the build it's running.
pub const API_VERSION: u32 = 3;

/// The host's file name. While it's loaded, a mod leaves starting and stopping to it.
pub const HOST_MODULE: &str = "primordialis_qol_hot_reload.dll";

/// `extern "C" fn() -> u32`: returns `API_VERSION`. Has this signature in every version.
pub const EXPORT_API_VERSION: &CStr = c"modkit_api_version";
/// `unsafe extern "C" fn(home_dir: *const u16, home_dir_len: usize) -> bool`: loads settings and
/// finds what to hook, without changing the game. `home_dir` (UTF-16, not NUL-terminated) holds the
/// log, settings and caches. Returns whether `EXPORT_START` can be called; if not, unload the mod.
pub const EXPORT_PREPARE: &CStr = c"modkit_prepare";
/// `extern "C" fn() -> bool`: hooks the game. Returns whether the mod is active; if not, none of its
/// code runs and it can be unloaded.
pub const EXPORT_START: &CStr = c"modkit_start";
/// `extern "C" fn() -> bool`: removes the hooks and undoes the mod's changes once no thread is
/// inside it. Returns whether it can be unloaded; if not, it keeps running.
pub const EXPORT_STOP: &CStr = c"modkit_stop";

pub type ApiVersion = unsafe extern "C" fn() -> u32;
pub type Prepare = unsafe extern "C" fn(*const u16, usize) -> bool;
pub type Start = unsafe extern "C" fn() -> bool;
pub type Stop = unsafe extern "C" fn() -> bool;
