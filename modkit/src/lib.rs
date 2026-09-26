//! A framework for Primordialis mods. A mod is a list of [`Feature`]s: each reacts to what the game
//! is rendering, and reads and draws through safe [`game`] bindings. Everything underneath is
//! handled here:
//!
//! - **Loading**: [`entry!`] turns a crate into a mod DLL, loaded by the game's `--customdll` launch
//!   option or by the hot reload host, which swaps in new builds while the game runs.
//! - **Game bindings** ([`game`]): functions, globals and struct layouts are looked up by name in the
//!   debug symbols the game ships, so nothing is tied to one game build. If anything doesn't match,
//!   the mod stays off and the game runs unmodified.
//! - **Hooks**: installed and removed while the game runs, only when no thread is executing the code
//!   being patched, and removed only once no thread is inside the mod.
//! - **Settings** ([`settings`]): declared by features, kept in a TOML file next to the DLL that is
//!   completed with documented defaults and reloaded when it changes.
//! - **Isolation**: a feature that panics is turned off (and its changes undone) while the rest keep
//!   running. The mod allocates from its own heap, freed when the mod is unloaded.
//!
//! Modules are split by concern: [`game`] knows the game, and nothing about hooking or loading;
//! `hook`, `freeze` and `alloc` know how to patch a running process, and nothing about the game;
//! `events` and `lifecycle` connect the two to the features.

mod alloc;
mod events;
mod feature;
mod freeze;
pub mod game;
mod hook;
mod lifecycle;
pub mod log;
mod module;
pub mod settings;

pub use feature::Feature;
pub use lifecycle::Mod;

type Result<T> = std::result::Result<T, String>;

// The mod's allocator, for this crate's own tests: the hooking tests allocate while threads are
// paused. Mods get it from `entry!`.
#[cfg(test)]
#[global_allocator]
static TEST_ALLOCATOR: alloc::PrivateHeap = alloc::PrivateHeap;

/// Turns the crate into a mod DLL: defines its entry points (`DllMain`, and the exports the hot
/// reload host calls), and makes it allocate from its own heap. Call it once, in the crate root of a
/// `cdylib`, with the mod's [`Mod`] definition:
///
/// ```ignore
/// modkit::entry!(modkit::Mod {
///     name: "my_mod",
///     title: "My mod",
///     version: env!("CARGO_PKG_VERSION"),
///     homepage: env!("CARGO_PKG_REPOSITORY"),
///     features: || vec![Box::new(MyFeature::default())],
/// });
/// ```
#[macro_export]
macro_rules! entry {
    ($definition:expr $(,)?) => {
        #[global_allocator]
        static __MODKIT_ALLOCATOR: $crate::__private::PrivateHeap = $crate::__private::PrivateHeap;

        static __MODKIT_MOD: $crate::Mod = $definition;

        #[unsafe(no_mangle)]
        // The signature is dictated by Windows; the loader always passes our own module handle.
        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        pub extern "system" fn DllMain(
            module: $crate::__private::HMODULE,
            reason: u32,
            reserved: *mut ::core::ffi::c_void,
        ) -> $crate::__private::BOOL {
            // SAFETY: Called by Windows as `DllMain`.
            unsafe { $crate::__private::dll_main(&__MODKIT_MOD, module, reason, reserved) }
        }

        // The export names are `modkit_protocol::EXPORT_*`.
        #[unsafe(no_mangle)]
        pub extern "C" fn modkit_api_version() -> u32 {
            $crate::__private::API_VERSION
        }

        /// # Safety
        ///
        /// See `modkit_protocol::EXPORT_PREPARE`.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn modkit_prepare(home_dir: *const u16, home_dir_len: usize) -> bool {
            // SAFETY: Forwarded from the host.
            unsafe { $crate::__private::prepare(&__MODKIT_MOD, home_dir, home_dir_len) }
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn modkit_start() -> bool {
            $crate::__private::start()
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn modkit_stop() -> bool {
            $crate::__private::stop()
        }
    };
}

/// Used by `entry!`; not part of the API.
#[doc(hidden)]
pub mod __private {
    pub use crate::alloc::PrivateHeap;
    pub use crate::lifecycle::{dll_main, prepare, start, stop};
    pub use modkit_protocol::API_VERSION;
    pub use windows_sys::Win32::Foundation::HMODULE;
    pub use windows_sys::core::BOOL;
}

#[cfg(test)]
mod tests {
    /// `entry!` names its exports literally; they must be the ones the host looks for.
    #[test]
    fn entry_exports_the_protocol_names() {
        let source = include_str!("lib.rs");
        for name in [
            modkit_protocol::EXPORT_API_VERSION,
            modkit_protocol::EXPORT_PREPARE,
            modkit_protocol::EXPORT_START,
            modkit_protocol::EXPORT_STOP,
        ] {
            let name = name.to_str().unwrap();
            assert!(
                source.contains(&format!("extern \"C\" fn {name}(")),
                "entry! doesn't define {name}"
            );
        }
    }
}
