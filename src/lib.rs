//! Primordialis QoL mod: shows the icon of every cell pickup on the map, for areas you have explored.
//!
//! Loaded by the game itself through its `--customdll "primordialis_qol.dll"` launch option (or by the
//! hot reload host during development). Everything about loading, hooking and reading the game is
//! handled by `modkit`; this crate is just the features.

mod echolocation;
mod map_icons;
mod settle;

modkit::entry!(modkit::Mod {
    name: "primordialis_qol",
    title: "Primordialis QoL",
    version: env!("CARGO_PKG_VERSION"),
    features: || vec![
        Box::new(map_icons::MapIcons::default()),
        Box::new(echolocation::EcholocationFix::default())
    ],
});
