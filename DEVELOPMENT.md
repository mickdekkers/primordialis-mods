# Development

How Primordialis QoL works, and how to build, test and release it. For what the mod does and how to
install it, see the [README](README.md).

## How it works

- **Loading.** The game's `--customdll` launch option loads the DLL with `LoadLibraryW` early in
  startup, before the game renders anything. The mod hooks the game right away, from `DllMain`.
- **Finding things in the game.** Nothing is hardcoded to a game build. The mod reads the game
  executable's CodeView record to learn which PDB it was built with, extracts that PDB from the game's
  `pdbs.zip` into `primordialis_qol_cache` (once per game version, removing older ones), and uses
  Windows' DbgHelp to look up the functions, globals and struct layouts it needs by name. Each field is
  checked at the size the mod uses it at, so if a game update changes something the mod relies on, the
  mod turns itself off and says why in the log instead of reading the wrong memory.
- **Hooks.** It hooks game functions with [`detour`](https://crates.io/crates/detour):
  `render_game`, for the camera, the UI and the mouse of the frame being rendered,
  `begin_trace_stage`, which marks the start of each rendering stage, and `do_text_button`, which
  draws the menus' buttons. When the `menus` stage begins, the game has just drawn its map markers and
  is about to draw menus on top: that's where the icons are drawn, with the game's own
  `draw_cell_icons`.
- **Which pickups are shown.** A pickup is shown once the player has found it, by coming close enough
  to see it: within the fog of war's radius (`w.vision_radius`, the player's body's, 1000 by default)
  where the map's light (the game's `light_value`) is normal (0.5) or brighter, closing in to 400 units
  in the dark (light 0). That's checked every frame, also with the map open, since the player can move
  then too, and a newly found pickup's icon fades in over half a second. Only the light while the
  player is near counts, so lighting up a spot later (a boss's death makes walls glow) reveals nothing
  by itself. The map's own `explored` data isn't used: the game explores around its camera whether
  it's lit or not, and a sandbox counts its whole map as explored.
- **Remembering found pickups.** Pickups have no ids, so found ones are kept as their material's id
  and their position, followed as they move, and matched up again when the pickups change: one that
  disappears was picked up, unless many disappear at once (the game clearing the world). They're saved
  every few seconds while they change, with postcard, to `primordialis_qol_detected.bin`. Like the
  game's saves, the file keeps one normal run and one sandbox, told apart by the save the game uses
  (`saver.save_dir`). A run is identified by its save, seed and start time (`w.run.start_time`, which
  the game saves with it), so a new run replaces the old one of its kind, even on the same seed.
- **The version in the menus.** The main and pause menus show the game's version as a button, with
  its text centered. Just before `do_text_button` draws it, the mod measures the text with the game's
  `get_text_size` and moves the button up by a line, then draws its own version where it was with
  `draw_text`, in the button's font and color, starting where the game's text starts.
- **Where they're shown.** The game only simulates pickups near the camera, so far-away pickups can
  still sit where they spawned, inside rock, until its physics pushes them out as you approach. The mod
  applies the same push-out, with the game's own wall distance field, so icons show where pickups will
  end up. With `fix_echolocation_positions`, pickups are moved there while Echolocation draws its
  markers, and moved back right after.
- **Spreading out piles.** Overlapping icons are laid out on a hexagonal grid built around the icon
  under the mouse, which stays open while the mouse is over it. Their pickups are made transparent while
  the game draws pickups in the world, and moved out of range while Echolocation draws its markers, then
  restored.
- **Tooltips.** The tooltip is drawn with the game's own `do_tooltip`, given the same arguments the game
  gives it for the pickup under the mouse in the world.
- **Isolation.** If a feature panics, it's turned off and its changes are undone, while the rest of the
  mod and the game keep running. The mod allocates from its own heap, and never goes online (DbgHelp only
  searches the cache folder).

[`CLAUDE.md`](CLAUDE.md) has the rules the code follows to hook and unhook safely while the game runs.

## Project layout

The mod is built on `modkit`, a small framework that handles everything except the features
themselves.

| Crate | What it is |
|---|---|
| `primordialis_qol` (`src/`) | The mod: just its features. `map_icons.rs` draws the icons of the cells `found_cells/` says the player has found (with `spread/` laying out the grid, `fades.rs`, `leaders.rs` and `combo.rs`), `hide_duplicates.rs` hides what the map already shows, `echolocation.rs` fixes the Echolocation markers, and `menu_version.rs` shows the mod's version in the menus. Shared between them: `settle.rs` caches where pickups end up, `grid_pickups.rs` holds the pickups on the grid, `pickup_edits.rs` undoes changes to pickups, and `math.rs` has easing and timing. |
| `modkit` (`modkit/`) | The framework. `game/` holds safe bindings to the game (pickups, materials, map, camera, drawing, menus and tooltips), resolved from the game's symbols, and the values copied from the game's code. Beneath that sit loading, hooking and unhooking the running game, the mod's private heap, settings, logging, and the files kept next to the DLL. |
| `modkit_protocol` (`protocol/`) | The entry points the mod exports for the hot reload host, shared by both. |
| `primordialis_qol_hot_reload` (`hot_reload/`) | The hot reload host and injector (development only, never released). |

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

Settings a feature declares show up in `primordialis_qol.toml` with their description. Also add them to
the settings table in the README. A feature that changes game state temporarily undoes it in `revert`,
which is called before the mod unloads or if the feature panics. When a feature needs something the game
bindings don't have yet, add it to `modkit/src/game/`: resolve it by name in `bindings.rs`, then expose
it through a safe accessor.

## Building

Requires Rust (stable, MSVC toolchain) on Windows, with the Windows SDK that Visual Studio's C++ build
tools install: `build.rs` compiles the DLL's version information (name, version, description and
copyright, shown in the file's Properties) with the SDK's `rc.exe`.

```
cargo build --release
```

The DLL is `target/release/primordialis_qol.dll`. Don't hand out local builds: Rust embeds source paths
in the DLL for panic messages, and those include your user folder (where Cargo keeps downloaded crates).
Release builds are made by CI, which strips them (see [Releasing](#releasing)).

Tests: `cargo test --release`. Two more tests are ignored by default:

```
# Symbol resolution against an installed game
PRIMORDIALIS_DIR="<game folder>" cargo test --release -p modkit -- --ignored --nocapture resolves_against_installed_game
# Hooking and unhooking code other threads are running (pauses the test's other threads: run alone)
cargo test --release -p modkit -- --ignored --test-threads=1 patches_code_other_threads_are_running
```

Before committing: `cargo fmt --all`, and `cargo clippy --workspace --all-targets --release` should be
clean. CI (`.github/workflows/ci.yml`) checks formatting, clippy and the tests on every push to `main`
and every pull request.

Dependencies are pinned to exact, reviewed versions, in the workspace's `Cargo.toml` (and `Cargo.lock`).

### The workflows

CI also checks the workflows themselves, with two tools it downloads as release binaries pinned by
version and SHA-256 (in `ci.yml`):

- [pinact](https://github.com/suzuki-shunsuke/pinact) checks that every action is pinned to a full commit
  SHA, and that the version comment next to it names that commit. Its settings are in `.pinact.yaml`.
- [zizmor](https://docs.zizmor.sh) looks for security problems: injectable expressions, credentials left
  in checkouts, overly broad permissions, secrets outside an environment, actions with known
  vulnerabilities, and more.

After changing a workflow, run both locally (`pinact run --check --verify-comment` and
`zizmor --persona=pedantic .`). To update the actions, run `pinact run --update`: it only moves to
releases at least 30 days old. To update pinact or zizmor themselves, change the URL and the SHA-256 in
`ci.yml` together, taking the SHA-256 from the release's assets on GitHub.

## Hot reload

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

## Releasing

### Recording changes

Every change players would notice gets a change file, committed with the change itself, made with
[changie](https://changie.dev):

```
changie new
```

It asks for the kind (Added, Changed, Removed or Fixed) and a one-line description, written for players:
it goes into the changelog and the release notes as it is. Changes players won't notice (refactoring,
docs, CI) don't need one. The files wait in `.changes/unreleased/` until the next release; CI checks
that they're valid, and that `CHANGELOG.md` is what changie makes of the released ones (don't edit it
by hand).

### Making a release

Run the Release workflow on `main`, from the Actions tab or with:

```
gh workflow run release.yml
```

It picks the version from the kinds of the waiting changes: a new minor version for anything Added,
Changed or Removed, a patch version if there are only fixes. To choose it yourself, pass `major`, `minor`,
`patch` or a version (`gh workflow run release.yml -f version=1.0.0`). The workflow then:

1. Batches the waiting changes into the new version with changie: its section in `CHANGELOG.md`, and its
   number in `Cargo.toml` and `Cargo.lock`. It commits that as "Release v<version>", without pushing it
   yet. It fails if there's nothing to release.
2. Runs the tests, and builds `primordialis_qol.dll`, both with `--remap-path-prefix` (the same flags, so
   the build reuses the dependencies the tests compiled). The paths Rust embeds then point to `/cargo`,
   `/rustup` and `/build` instead of folders on the build machine. Nothing comes from a cache: every
   release is built from scratch. Then
   `.github/scripts/check-paths.ps1` checks that every path in the DLL has an expected form: no absolute
   paths at all, and every source path under one of those neutral roots (or the ones the Rust project's
   own builds use). Anything else fails the release. Run it on a local build to see what it catches.
3. Uploads the DLL to VirusTotal and waits for the scan (usually a few minutes). The release commit then
   gets the result: the VirusTotal badge in the README links to this version's report, so the README at
   each tag matches that release. The README isn't part of the build, so the DLL is still what that
   commit builds. Step 1 checks the README has that badge, so a missing one fails before the upload.
4. Pushes the release commit to `main`. It only fast-forwards: if `main` moved while the release was
   being built, nothing is pushed or released, and running the workflow again starts over from the new
   `main`.
5. Creates the tag and the GitHub release: the version's changelog section, a VirusTotal badge linking to
   the full report, the DLL's SHA-256, the DLL, and a zip with its PDB for investigating crashes.

If no engine flagged the DLL, the release is published. Otherwise, or if the scan didn't finish, it's
left as a draft: look at the report, adjust the notes if needed, and publish it by hand, which also
creates its tag. Heuristic engines sometimes flag DLLs that patch another program's code, which is
exactly what a mod does.

Only the mod is released. The hot reload host and injector are development tools, and an injector is
the kind of program antivirus rightly distrusts.
