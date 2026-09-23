# Primordialis QoL: map pickup icons

When you open the map, every cell pickup in an area you have explored is shown with its cell icon, so
you can see *which* cells are lying around, not just where. The icons are drawn by the game's own icon
renderer, so they look like the pickups do in the world, and they fade in and out with the map.
Combo pickups get the game's combo coloring plus a ring of rainbow dots, standing in for the particle
ring they have in the world.

## Install

1. Download `primordialis_qol.dll` and put it in the game folder, next to `primordialis.exe`.
   (In Steam: right-click Primordialis → Manage → Browse local files.)
2. In Steam: right-click Primordialis → Properties → General → Launch Options, and enter:

   ```
   --customdll "primordialis_qol.dll"
   ```
3. Start the game.

`--customdll` is the game's own option for loading a DLL; no game files are modified. The mod writes
`primordialis_qol.toml` (settings), `primordialis_qol.log` and a `primordialis_qol_cache` folder next to
the DLL.

## Settings

On first start the mod creates `primordialis_qol.toml` next to the DLL, with every setting, its
description and its default value. Changes apply while the game is running. Settings missing from the
file (for example, ones added in a newer version of the mod) are added back with their defaults.

| Setting | Default | Effect |
|---|---|---|
| `fix_icon_positions` | `true` | Shows map icons where pickups will actually be, not inside rock where they spawned (see below). |
| `fix_echolocation_positions` | `true` | Also moves the Echolocation mutation's pickup markers out of rock, to where the pickups will actually be (see below). |

## Uninstall

Remove the launch option. Optionally delete `primordialis_qol.dll`, `primordialis_qol.toml`,
`primordialis_qol.log` and the `primordialis_qol_cache` folder.

## Compatibility

The mod doesn't hardcode anything about a specific game version. On startup it finds everything it
needs by name in the debug symbols that ship with the game (`pdbs.zip`), and checks the layouts it relies
on. It works with both the AVX and SSE3 builds of the game.

If a game update changes something the mod depends on, the mod turns itself off and the game runs
normally. The reason is written to `primordialis_qol.log`.

## How it works

- On load, it reads the game executable's debug record, extracts the matching `.pdb` from `pdbs.zip` into
  `primordialis_qol_cache` (once per game version), and uses Windows' DbgHelp to look up the functions,
  variables and struct layouts it needs.
- It hooks two game functions with [`detour`](https://crates.io/crates/detour): `render_game` (to get the
  camera of the frame being rendered) and `begin_trace_stage`, which marks the start of each rendering
  stage. At the start of the `menus` stage, the game has just drawn its other map markers above the fog
  of war, and is about to draw menus on top. That's where the icons are drawn, using the game's
  `draw_cell_icons`.
- A pickup is shown when the map hex it's in has been explored (the same data the map uses to reveal
  walls).
- The game only simulates pickups near you, so far-away pickups can still sit where they spawned,
  inside rock, until the physics pushes them out as you approach. The mod applies that same push-out
  (with the game's own wall distance field) so icons show where the pickups will actually be. The
  game's Echolocation markers don't do this, unless `fix_echolocation_positions` is on: then pickups are moved
  there while Echolocation draws its markers, and moved back right after.

## Building

Requires Rust (stable, MSVC toolchain) on Windows.

```
cargo build --release
```

The DLL is `target/release/primordialis_qol.dll`. For development, point the launch option at it
directly: `--customdll "<path to repo>\target\release\primordialis_qol.dll"`.

Tests: `cargo test --release`. To also check symbol resolution against an installed game:

```
PRIMORDIALIS_DIR="<game folder>" cargo test --release -- --ignored --nocapture
```

Dependencies are pinned to exact, reviewed versions (see `Cargo.toml` and `Cargo.lock`).
