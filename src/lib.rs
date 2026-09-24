//! Primordialis QoL mod: shows the icon of every cell pickup on the map, for areas you have explored,
//! spreads out icons that overlap under the mouse and shows the game's tooltip for the one it points
//! at, and shows those icons and the Echolocation mutation's markers where pickups will settle.
//!
//! Loaded by the game itself through its `--customdll "primordialis_qol.dll"` launch option (or by the
//! hot reload host during development). Everything about loading, hooking and reading the game is
//! handled by `modkit`; this crate is just the features.

mod combo;
mod echolocation;
mod fades;
mod grid_pickups;
mod hide_duplicates;
mod leaders;
mod map_icons;
mod math;
mod settle;
mod spread;
mod tooltip;

modkit::entry!(modkit::Mod {
    name: "primordialis_qol",
    title: "Primordialis QoL",
    version: env!("CARGO_PKG_VERSION"),
    features: || {
        let grid = grid_pickups::GridPickups::default();
        vec![
            Box::new(map_icons::MapIcons::new(grid.clone())),
            Box::new(hide_duplicates::HideDuplicates::new(grid.clone())),
            Box::new(echolocation::EcholocationFix::new(grid)),
        ]
    },
});
