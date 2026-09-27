//! Primordialis QoL mod: shows the icon of every cell pickup on the map that you've been close enough
//! to see (less close where it's lit), spreads out icons that overlap under the mouse and
//! shows the game's tooltip for the one it points at, and shows those icons and the Echolocation
//! mutation's markers where pickups will settle. Its version shows under the game's in the menus.
//!
//! Loaded by the game itself through its `--customdll "primordialis_qol.dll"` launch option (or by the
//! hot reload host during development). Everything about loading, hooking and reading the game is
//! handled by `modkit`; this crate is just the features.

mod combo;
mod echolocation;
mod fades;
mod found_cells;
mod grid_pickups;
mod hide_duplicates;
mod leaders;
mod map_icons;
mod math;
mod menu_version;
mod settle;
mod spread;

modkit::entry!(modkit::Mod {
    name: "primordialis_qol",
    title: "Primordialis QoL",
    version: env!("CARGO_PKG_VERSION"),
    homepage: env!("CARGO_PKG_REPOSITORY"),
    features: || {
        let grid = grid_pickups::GridPickups::default();
        vec![
            Box::new(map_icons::MapIcons::new(grid.clone())),
            Box::new(hide_duplicates::HideDuplicates::new(grid.clone())),
            Box::new(echolocation::EcholocationFix::new(grid)),
            Box::new(menu_version::MenuVersion),
        ]
    },
});
