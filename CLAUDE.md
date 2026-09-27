# Primordialis QoL mod

A Rust mod for the game Primordialis (Windows, x64). It shows cell pickup icons on the map, and moves
the Echolocation mutation's markers out of rock. Players install it with the game's own
`--customdll "primordialis_qol.dll"` launch option; no game files are modified.

## Architecture

- **`modkit`**: the framework. Everything except the features lives here.
  - `game/`: safe bindings to the game. It knows the game but nothing about hooking.
  - The rest: the hook engine, thread pausing (`freeze`), the private heap (`alloc`), settings,
    logging, and the load/start/stop lifecycle. None of it knows anything about the game.
  - `events` connects the game's render hooks to the features.
- **`modkit_protocol`** (`protocol/`): the exports a mod provides to the hot reload host, and
  `API_VERSION`. Shared by both, so they can't drift apart.
- **`primordialis_qol`** (`src/`): the mod itself, which is only features implementing
  `modkit::Feature`. Feature code should stay free of `unsafe`. If a feature needs something new from
  the game, add a binding in `modkit/src/game/`: resolve it by name in `bindings.rs`, then expose it
  through a safe accessor. A binding the features can do without is `Optional`: if a game update
  breaks it, the mod still starts, and its accessor returns `None` (or its hook isn't installed)
  instead of turning the whole mod off. Don't bind what no feature uses.
- **`primordialis_qol_hot_reload`** (`hot_reload/`): a development-only host DLL that swaps in new
  mod builds while the game runs, plus `primordialis_qol_inject.exe`, which loads the host into a game
  that is already running.

## How the game is accessed

- **Symbols:** nothing is hardcoded to a game build. Functions, globals and struct layouts are
  resolved by name with DbgHelp, from the PDB matching the running exe. That PDB is extracted from the
  game's `pdbs.zip`, which contains full private PDBs.
- **Mirrored types:** every `#[repr(C)]` mirror of a game type must be checked against the PDB in
  `bindings.rs`.
- **Field sizes:** every field read or written is resolved with the size it's used as
  (`TypeLayout::offset_of::<T>`), so a field whose type changed turns the mod off instead of being
  read or overwritten along with its neighbors.
- **Values taken from game code:** some values are constants compiled into the game's code rather
  than symbols, such as the combo colour cycle. They are copied by hand and must be documented as
  such.
- **Hook points:**
  - `render_game(world_rc, ui_rc, input, ...)`: frames, the world render context (camera), the UI
    render context (UI camera, fonts), and the input. The mouse is in UI units: the screen height
    spans -1 to 1, y up.
  - `begin_trace_stage(name)`: stage boundaries. The `racing_overlay` stage (Echolocation markers,
    and the game's tooltip for the pickup under the mouse in the world) comes right before `menus`. When `menus` begins, the UI framebuffer is bound and the map markers
    have just been drawn: that's where the mod draws.
  - `do_text_button(rc, input, pos, half_size, text)`: the main and pause menus' buttons (both menus
    are drawn by `do_pause_menu`, inside `render_game`). Features get a `MenuButton` to move, or to
    add labels to (`Feature::menu_button`). Only hooked if its bindings resolve.
  - Those are the only two stages that read the pickups one by one: `racing_overlay` draws a marker
    for every pickup within range of the camera, and `cell pickups` queues the pickups near the
    camera to be drawn in the world, faded by `cell_pickup.alpha`. Features change pickups for one
    of these stages only, and change them back when it ends (and in `revert`).
- **Features sharing state:** features are `Send` and separate, so shared state lives in an
  `Arc<Mutex<_>>` created in `entry!`'s `features` (see `src/grid_pickups.rs`). It's only locked in
  stage callbacks, with `try_lock`, and never in `revert` (which can use atomics, like
  `GridPickups::withdraw`). The map icons set the grid in `menus`, so the features that read it in
  `cell pickups` and `racing_overlay` see the previous frame's.
- **Simulation clock:** `w.frame_number` counts simulation steps at a fixed 120 per second, so it's
  frame-rate independent. Use it for animation timing.
- **Map light:** each biome lights its hexes of `map.light` (`biome_type.light`: 0.5 by default, 0.3
  to 1), and the darkness modifier (`biome_darkness`) sets its hexes to 0 and leaves light cells at the
  entrances. The game's `light_value` blends it between hexes (`Map::light_at`), and the light changes
  at runtime (`set_glowing_walls` runs when a boss dies).
- **Exploring and sight:** exploring ignores light: `update_cells` explores every hex around
  `w.camera_pos` (`Game::view_center`), lit or not, within a hardcoded 1000 units (3000 with
  Echolocation), and a sandbox counts its whole map as explored (every `explored` value is 1), so the
  mod doesn't use `explored`. The fog of war on the map is a different radius: `w.vision_radius`
  (`Game::vision_radius`), copied from the player's body, clear within 80% of it (`walls.glsl`).
  `src/found_cells` shows a pickup only once the player came within that radius of it (less in the
  dark), map open or not (the player can move with it open), the same way in normal runs and
  sandboxes. It remembers found ones in `primordialis_qol_detected.bin` (`modkit::storage`), per save
  slot (`Game::save_slot`) and run.
- **Pickups:**
  - The game only simulates pickups near the camera; far away they can sit inside rock.
  - `is_combo` pickups only come from map generation. The sandbox combo tool can't produce one.
    Placing another cell type next to one merges them into a combo cell ("Combo Cell").
- **Overloads:** some game functions are C++ overloads sharing a name (such as `draw_line`). Resolve
  those with `Symbols::function(name, params)`.

## Invariants

These keep hooking and unloading safe while the game runs:

- **No locks or process-heap allocation while threads are paused.** Code that runs while the game's
  threads are paused (`freeze::while_paused` callbacks, `Feature::revert` during unload) must not
  take locks a game thread could hold, and must not log. It also must not allocate from anything but
  the mod's private heap, which is the global allocator installed by `entry!`.
- **Detours count themselves first.** Every detour starts with `hook::InFlight::enter()`. Unhooking
  waits for zero in-flight calls, and for no paused thread to be inside the mod, its trampolines, or
  the patched prologues.
- **Nothing may outlive an unload.** Thread-locals must be `const`-initialized with no destructor. A
  feature that changes game state temporarily undoes it in `revert`.
- **Game views stay inside their callback.** `Game` and `Frame` are `!Send`, and borrow from the
  callback they were handed to.
- **Never fail `DllMain` or panic into the game.** On any error, log it and let the game run
  unmodified. Features that panic are turned off on their own.
- **Changing the host exports means bumping `modkit_protocol::API_VERSION`.** Hosts refuse
  mismatched builds and keep the build that's running.

## Building and testing

- `cargo build --release` builds the mod (`target/release/primordialis_qol.dll`), not the host.
- `cargo build --release --workspace` also builds the host. Only do this with the game closed: the
  running game locks the host DLL.
- `cargo test --release` runs the default members (the mod, modkit, protocol). There are two ignored
  tests in `modkit`:
  - `PRIMORDIALIS_DIR="<game dir>" cargo test --release -p modkit -- --ignored --nocapture resolves_against_installed_game`
  - `cargo test --release -p modkit -- --ignored --test-threads=1 patches_code_other_threads_are_running`
    (pauses every other thread, so run it alone).
- `cargo clippy --workspace --all-targets --release` should be clean.
- Run `cargo fmt --all` before every commit. It uses rustfmt's defaults (there is no rustfmt
  config), and `cargo fmt --all --check` must be clean.
- The Rust sources and the other docs use LF line endings, and `README.md` uses CRLF. Scripted edits
  must keep each file's line endings (in Python on Windows, open files with `newline=''`).
- The game comes in AVX and SSE3 builds (`primordialis_avx.exe`, `primordialis_sse3.exe`).
- **Logs** go next to the DLL: `primordialis_qol.log` for the mod, `primordialis_qol_hot_reload.log`
  for the host. Each game process starts a new mod log and keeps the previous one as
  `primordialis_qol.log.bak`; builds the host swaps in append to it. Settings are in
  `primordialis_qol.toml`, symbols are cached in `primordialis_qol_cache/`.
- With the hot reload host loaded (see `DEVELOPMENT.md`), every `cargo build --release` of the mod is
  swapped into the running game, so build once the edits are complete. `cargo check`, `clippy` and
  `test` don't write the DLL.

## Docs

- **`README.md` is for players, not programmers:** what the mod does and why, how to install it, and
  whether it's safe. How things work goes in `DEVELOPMENT.md`. Keep the README's settings table in
  line with the settings the features declare.
- **Changes players would notice get a changie fragment**, committed with the change:
  `changie new -k <Added|Changed|Removed|Fixed> -b "<one line, written for players>" -i=false`. The
  line becomes the changelog entry and the release notes as it is. Refactors, docs and CI don't get
  one. `CHANGELOG.md` is generated by changie at release time: never edit it by hand (CI checks).
- **Screenshots** for the README go in `docs/screenshots/`, under the names the README links to.

## Releasing

- Releases are made by running `.github/workflows/release.yml` on `main` (`gh workflow run
  release.yml`, optionally `-f version=...`). It batches the fragments with changie (which also sets
  the version in `Cargo.toml` and `Cargo.lock`), builds and tests, scans the DLL on VirusTotal, then
  pushes the "Release v<version>" commit to `main` and creates the tag and release (a draft if any
  engine flagged it). Never bump the version or tag by hand. `DEVELOPMENT.md` has the details.
- Only `primordialis_qol.dll` (and a zip of its PDB) is released, never the hot reload host or injector.
  Its file name is what players type in the launch option, so it never changes.
- Release builds strip local paths with `--remap-path-prefix`, and `.github/scripts/check-paths.ps1`
  fails the release unless every path in the DLL has an expected form (an allowlist, not a search for
  known names). Local builds contain the home folder of whoever built them, so they are never
  distributed.
- `build.rs` embeds the DLL's version resource with `embed-resource` (which runs the SDK's `rc.exe`),
  from the package's metadata and the copyright line in `LICENSE`.
- **Committed files are for everyone:** assume other people read every one of them. No paths from this
  machine, user names or emails, and no private notes to the maintainer either (setup reminders,
  account or repository settings, to-dos). Those go in `CLAUDE.local.md`, which isn't committed.
- Actions are pinned by commit SHA (with the version in a comment), and the Rust toolchain is pinned in
  the workflows' `RUSTUP_TOOLCHAIN`. Update them deliberately, like dependencies.
- CI checks the workflows with pinact (`.pinact.yaml`) and zizmor, and the change fragments with
  changie, all downloaded by version and SHA-256 (changie in both `ci.yml` and `release.yml`). After editing a workflow, run `pinact run --check --verify-comment` and
  `zizmor --persona=pedantic .` locally; zizmor's `auditor` persona should stay clean too.
- Jobs that use secrets run in an environment (zizmor checks this). Every job gets the least
  `permissions` it needs, with a comment saying why for anything beyond `contents: read`.

## Dependencies

- **Review:** every new crate, including every transitive dependency, is reviewed before it's added.
  Prefer a solid, well-regarded crate to writing one yourself.
- **Pins:** versions are pinned exactly in `[workspace.dependencies]` in the root `Cargo.toml`.
  Don't loosen them.
- **Checking what a change pulls in:** preview it with `cargo update --workspace --dry-run`, and
  build with `--offline` to prove nothing new was fetched.
- **Build dependencies count too:** `embed-resource` and its tree (including `cc`, `vswhom-sys`,
  `winreg` and windows-sys 0.59) run on the build machine, and so do the proc macros postcard needs
  (serde's `derive` isn't optional for it: `serde_derive`, `thiserror-impl`, `syn`). `cc`,
  `find-msvc-tools`, `thiserror`, `syn` and `unicode-ident` are held below their newest releases in
  `Cargo.lock` (releases younger than 30 days aren't used yet), so `cargo update` would move them.

## Style

- Doc comments are plain prose that explain why.
- Every `unsafe` block has a `// SAFETY:` comment.
- Errors are `String`s with context, e.g. `format!("cannot hook {name}: {e}")`.
- Commit messages have a summary line and then a body explaining why. Work happens on `main`. Run
  `cargo fmt --all` first.
