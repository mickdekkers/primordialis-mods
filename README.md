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
- If a feature ever crashes, it turns itself off (undoing its changes) and the rest of the mod and
  the game keep running.
- A pickup is shown when the map hex it's in has been explored (the same data the map uses to reveal
  walls).
- The game only simulates pickups near you, so far-away pickups can still sit where they spawned,
  inside rock, until the physics pushes them out as you approach. The mod applies that same push-out
  (with the game's own wall distance field) so icons show where the pickups will actually be. The
  game's Echolocation markers don't do this, unless `fix_echolocation_positions` is on: then pickups are moved
  there while Echolocation draws its markers, and moved back right after.

## Project layout

The mod is built on `modkit`, a small framework that handles everything except the features
themselves:

| Crate | What it is |
|---|---|
| `primordialis_qol` (`src/`) | The mod: just its features. `map_icons.rs` draws the icons, `echolocation.rs` fixes the Echolocation markers, and `settle.rs` (shared by both) works out where pickups end up. |
| `modkit` (`modkit/`) | The framework. `game/` holds safe bindings to the game (pickups, materials, map, camera, drawing), resolved from the game's symbols. Beneath that sit loading, hooking and unhooking the running game, the mod's private heap, settings and logging. |
| `modkit_protocol` (`protocol/`) | The entry points the mod exports for the hot reload host, shared by both. |
| `primordialis_qol_hot_reload` (`hot_reload/`) | The hot reload host and injector (development only). |

### Adding a feature

A feature is a type implementing `modkit::Feature`, added to the list in `src/lib.rs`. It is called
on the render thread as each rendering stage begins and ends. Each call gets a `Frame` with safe
access to the game and the game's own renderers, e.g.:

```rust
static SHOW_THING: Toggle = Toggle::new("show_thing", true, "Shows the thing on the map.");

#[derive(Default)]
pub struct Thing;

impl Feature for Thing {
    fn name(&self) -> &'static str { "thing" }
    fn settings(&self) -> Vec<&'static dyn Setting> { vec![&SHOW_THING] }
    fn stage_begin(&mut self, frame: &Frame, stage: Stage) {
        if stage == Stage::MENUS && SHOW_THING.get() && frame.game().map_open() {
            // Read frame.game().pickups(), frame.camera(), ...; draw with frame.draw_circles(...).
        }
    }
}
```

Settings a feature declares show up in `primordialis_qol.toml` with their description. A feature that
changes game state temporarily undoes it in `revert`, which is called before the mod unloads or if the
feature panics. When a feature needs something the game bindings don't have yet, add it to
`modkit/src/game/`: resolve it by name in `bindings.rs`, then expose it through a safe accessor.

## Building

Requires Rust (stable, MSVC toolchain) on Windows.

```
cargo build --release
```

The DLL is `target/release/primordialis_qol.dll`.

Tests: `cargo test --release`. Two more tests are ignored by default:

```
# Symbol resolution against an installed game
PRIMORDIALIS_DIR="<game folder>" cargo test --release -p modkit -- --ignored --nocapture resolves_against_installed_game
# Hooking and unhooking code other threads are running (pauses the test's other threads: run alone)
cargo test --release -p modkit -- --ignored --test-threads=1 patches_code_other_threads_are_running
```

## Hot reload (development)

The hot reload host swaps in each new build of the mod while the game keeps running:

1. Build everything once, with the game closed: `cargo build --release --workspace`.
2. Point the launch option at the host instead of the mod:
   `--customdll "<path to repo>\target\release\primordialis_qol_hot_reload.dll"`. For a game that is
   already running (started without it), run `target\release\primordialis_qol_inject.exe` instead.
3. Edit, then `cargo build --release` while the game runs. About a second after the build finishes,
   the running build removes its hooks, restores what it changed and unloads, and the new build takes
   its place.

The host loads copies of the mod from `target\release\hot_reload\`, so builds can overwrite the
original. It logs to `primordialis_qol_hot_reload.log`; the mod keeps logging to `primordialis_qol.log`.

- Hooks are only patched while the game's other threads are paused, and none of them is executing the
  code being patched. Before a build is unloaded, the host also waits until no thread is inside it.
- If a build can't be stopped safely within 10 seconds, it keeps running and the new one is discarded;
  rebuild to try again. If a new build fails to start, the previous one is started again.
- The mod allocates from its own heap, which is destroyed when a build is unloaded, so nothing stays
  behind in the game.
- A plain `cargo build --release` only builds the mod: the host is locked while the game runs. Rebuild
  the host (`-p primordialis_qol_hot_reload`) with the game closed. The host and the mod must agree on
  `modkit_protocol::API_VERSION`; a host refuses builds that don't, and keeps the running one.

Dependencies are pinned to exact, reviewed versions, in the workspace's `Cargo.toml` (and `Cargo.lock`).
