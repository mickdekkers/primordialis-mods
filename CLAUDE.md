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
  through a safe accessor.
- **`primordialis_qol_hot_reload`** (`hot_reload/`): a development-only host DLL that swaps in new
  mod builds while the game runs, plus `primordialis_qol_inject.exe`, which loads the host into a game
  that is already running.

## How the game is accessed

- **Symbols:** nothing is hardcoded to a game build. Functions, globals and struct layouts are
  resolved by name with DbgHelp, from the PDB matching the running exe. That PDB is extracted from the
  game's `pdbs.zip`, which contains full private PDBs.
- **Mirrored types:** every `#[repr(C)]` mirror of a game type must be checked against the PDB in
  `bindings.rs`.
- **Values taken from game code:** some values are constants compiled into the game's code rather
  than symbols, such as the combo colour cycle. They are copied by hand and must be documented as
  such.
- **Hook points:**
  - `render_game`: frames, and the world render context (camera).
  - `begin_trace_stage(name)`: stage boundaries. The `racing_overlay` stage (Echolocation markers)
    comes right before `menus`. When `menus` begins, the UI framebuffer is bound and the map markers
    have just been drawn: that's where the mod draws.
- **Simulation clock:** `w.frame_number` counts simulation steps at a fixed 120 per second, so it's
  frame-rate independent. Use it for animation timing.
- **Pickups:**
  - The game only simulates pickups near the camera; far away they can sit inside rock.
  - `is_combo` pickups only come from map generation. The sandbox combo tool can't produce one.

## Invariants

These keep hooking and unloading safe while the game runs:

- **No locks or process-heap allocation while threads are paused.** Code that runs while the game's
  threads are paused (`freeze::while_paused` callbacks, `Feature::revert` during unload) must not
  take locks a game thread could hold. It also must not allocate from anything but the mod's private
  heap, which is the global allocator installed by `entry!`.
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
- On this machine the game is installed at `G:\SteamLibrary\steamapps\common\Primordialis`. It comes
  in AVX and SSE3 builds (`primordialis_avx.exe`, `primordialis_sse3.exe`).

## Working with the user's running game

- **The dev setup:** the Steam launch option points at `target\release\primordialis_qol_hot_reload.dll`.
  The host loads copies of the mod from `target\release\hot_reload\` and swaps in each new build
  about a second after `cargo build --release` finishes.
- **Every build is a swap while the game runs.** Check whether it's running first
  (`tasklist | grep -i primordialis`), and ask before building if it is. `cargo check`, `clippy` and
  `test` don't write the DLL, so they are always safe.
- **Ask before writing to the game's memory** (e.g. with a debugger).
- **Logs** go next to the DLL: `primordialis_qol.log` for the mod, `primordialis_qol_hot_reload.log`
  for the host. Settings are in `primordialis_qol.toml`, symbols are cached in
  `primordialis_qol_cache/`.
- **Screenshots:** only capture the game window when it's in the foreground, and only with the user's
  consent.

## Dependencies

- **Approval:** the user approves every new crate, including every transitive dependency, *before*
  it is downloaded. Don't write a crate yourself when a solid, well-regarded one exists, but propose
  it for review first.
- **Pins:** versions are pinned exactly in `[workspace.dependencies]` in the root `Cargo.toml`.
  Don't loosen them.
- **Checking what a change pulls in:** preview it with `cargo update --workspace --dry-run`, and
  build with `--offline` to prove nothing new was fetched.

## Style

- Doc comments are plain prose that explain why.
- Every `unsafe` block has a `// SAFETY:` comment.
- Errors are `String`s with context, e.g. `format!("cannot hook {name}: {e}")`.
- Commit messages have a summary line and then a body explaining why. Work happens on `main`. Run
  `cargo fmt --all` first.
