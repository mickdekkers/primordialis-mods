---
name: review-refactor
description: Comprehensive review of the whole repo (layering, safety invariants, correctness, simplification, docs, tests), then an approved, behaviour-preserving refactor in small commits.
disable-model-invocation: true
argument-hint: "[scope, e.g. 'modkit/src/game' or 'review only']"
---

# Review and refactor the repo

Scope: $ARGUMENTS (if empty: the whole workspace). "review only" means stop after phase 2.

You are reviewing a codebase that hooks into a live game process. A reviewer's job here is to find
what is **wrong**, what is **in the wrong place**, and what is **more complicated than it needs to
be**, in that order of priority. Taste-only rewrites are not findings. The codebase is already
opinionated (see `CLAUDE.md`); judge it against its own rules first, and against general Rust
practice second.

## Phase 1: read everything, establish a baseline

1. Read `CLAUDE.md`, `README.md`, the root `Cargo.toml`, and `git log --oneline -30`. Recent commits
   show which areas have churned; churned code is where leftovers accumulate.
2. Read **every** `.rs` file in scope in full. Don't skim or sample: the invariants below are
   violated in single lines.
3. Run and record the results (don't fix anything yet):
   - `cargo fmt --all --check`
   - `cargo clippy --workspace --all-targets --release`
   - `cargo test --release`
4. Build the module dependency graph from `use crate::…` / `use modkit::…` lines (grep is fine) and
   keep it at hand for the layering checks.

Do not run `cargo build --release` in this phase: every build is hot-swapped into the user's running
game.

## Phase 2: review

Work through every section. For each one, either report findings or state in one line that it's
clean; a clean section is a useful result, don't pad it.

### A. Separation of concerns (the layering in `CLAUDE.md`)

- **Dependency direction.** `modkit::game` knows the game but nothing about hooking or loading: it
  must not import `hook`, `freeze`, `alloc`, `lifecycle` or `events`. `hook`, `freeze`, `alloc`,
  `log`, `settings`, `module` know nothing about the game: they must not import `game`. Only
  `events` and `lifecycle` connect the two. Check with the graph, and flag every edge that points
  the wrong way.
- **Game knowledge that leaked out of `game/`.** Symbol names, struct offsets, stage names, game
  constants or game-specific units showing up in features or in the generic framework modules.
- **Framework knowledge that leaked into features.** `src/` should be features only: no `unsafe`, no
  `windows-sys`, no raw pointers, no knowledge of hooks or threads. Anything a feature needs from
  the game should be a safe accessor in `modkit/src/game/`.
- **Feature-specific code that leaked into `modkit`.** Things only one feature of this mod uses,
  shaped around that feature, that a second mod wouldn't want.
- **One concern per module.** For each file, write its responsibility in one sentence. If you need
  "and", check whether it should be split: e.g. a feature file that mixes state/animation, layout
  geometry, input handling and drawing; or a pure algorithm tangled with rendering. Conversely, flag
  modules so thin they're just indirection.
- **Coupling between features.** Shared state goes through an `Arc<Mutex<_>>` built in `entry!`'s
  `features` (see `grid_pickups`). Look for hidden coupling instead: features relying on each
  other's call order, on stage order, or on side effects the other leaves in game state.
- **Public API surface.** In `modkit`, is every `pub` item actually needed by the mod or the host?
  Prefer `pub(crate)`. `__private` stays `#[doc(hidden)]` and minimal. `protocol` holds only what
  the host and the mod must agree on. `hot_reload` should not depend on `modkit` internals.
- **Pure logic is testable.** Geometry, layout, easing, settle/push-out and settings parsing should
  be plain functions over plain data, testable without a running game. Flag pure logic that can
  only run inside a hook callback.

### B. Safety and the invariants in `CLAUDE.md`

Turn every invariant in `CLAUDE.md` into a concrete check and verify each occurrence, not a sample:

- Every `unsafe` block has a `// SAFETY:` comment, and the justification is **actually true** (read
  the call site: pointer validity, lifetimes, alignment, aliasing, thread). A wrong SAFETY comment
  is worse than none.
- Every `#[repr(C)]` mirror of a game type is verified against the PDB in `bindings.rs` (size, and
  the offset of every field read).
- Every detour starts with `hook::InFlight::enter()`, and no panic can unwind out of an
  `extern "C"`/`extern "system"` function into the game.
- Code reachable while threads are paused (`freeze::while_paused` callbacks, `Feature::revert`)
  takes no locks a game thread could hold, doesn't log if logging locks, doesn't allocate outside
  the private heap, and doesn't call game functions. Trace the calls, including through helpers.
- Thread-locals are `const`-initialized and have no destructor. Nothing (threads, handles, hooks,
  heap memory handed to the game) outlives an unload.
- Every temporary change to game state (pickup positions, flags, alpha) is undone at the end of the
  stage **and** in `revert`, including when the expected `stage_end` never comes (a stage skipped,
  the map closed mid-frame, the feature panicking between the change and the undo).
- Shared feature state is only locked in stage callbacks, with `try_lock`, never in `revert`.
- `DllMain` never fails; every error is logged with context and leaves the game unmodified.
- Host exports unchanged, or `API_VERSION` bumped.
- Values copied from game code are documented as such, with where they came from.
- Overloaded game functions are resolved with `Symbols::function(name, params)`.

### C. Correctness

- Frame-rate dependence: animation timing should use `frame_number` (120 steps/s), not wall time or
  per-frame increments. Check each use of `dt` is justified.
- Units: world units vs UI units (screen height spans -1..1, y up) vs pixels. Every conversion in
  one place, every constant's unit clear from its name or doc.
- Edge cases: no pickups, one pickup, thousands of pickups; extreme map zoom; mouse outside the map;
  the map opening/closing mid-animation; the game paused; NaN/inf from zero-length vectors or
  division by zero; integer overflow on `frame_number` wraparound.
- Stale state across frames: IDs or indices kept across frames when the pickup list can change
  (pickups collected, merged into combos, despawned, a new level loaded).
- Settings: reloading while running, invalid values, missing keys, and what happens to state that
  depends on a setting when it flips.

### D. Simplicity and efficiency

- Duplication: small helpers (distance, lerp, smoothstep, colour maths) defined in several modules;
  near-identical code paths that differ by one parameter. Say where the one shared copy should live
  (a feature-side helper module, or a method on a `modkit::game` type), and why there.
- Leftovers from iteration: dead code, unused constants or settings, tuning knobs nobody reads,
  comments describing behaviour that has since changed, `#[allow(...)]` that no longer applies.
- State that's modelled as scattered bools/options where an enum would make invalid states
  unrepresentable, and vice versa: machinery heavier than the problem.
- Hot path: this runs every frame on the render thread. Flag per-frame allocations, maps rebuilt
  from scratch every frame, and anything quadratic in the number of pickups, with a realistic
  estimate of n. Don't micro-optimize what can't matter.
- Magic numbers: named, with their unit and why that value.

### E. Docs, errors, style

- Module and item docs match what the code does now (compare against the recent commits), and
  explain *why* in plain prose.
- `README.md` matches reality: features described, the settings table vs the settings actually
  declared (names, defaults, descriptions).
- `CLAUDE.md` is still accurate. It's the brief every future session starts from, so a stale line
  there is a real finding.
- Errors are `String`s with context (`format!("cannot hook {name}: {e}")`); no bare `unwrap` on
  anything that depends on the game or the filesystem.

### F. Tests and tooling

- Which pure logic has no tests, and which of the refactors you're proposing need a test in place
  **before** they're done, as a safety net.
- Tests that assert implementation details instead of behaviour.
- `Cargo.toml`: pins still exact, no unused dependencies or features. Any dependency change needs
  the user's approval before anything is downloaded; propose, don't add.

## Report

Present the review before changing anything. Group findings by severity:

1. **Bugs and invariant violations**: things that can crash, corrupt, or leak into the game.
2. **Architecture**: layering and separation-of-concerns problems.
3. **Simplification**: duplication, leftovers, overcomplication.
4. **Docs, tests, tooling.**

Each finding: `file:line`, what's wrong, why it matters (a concrete failure scenario for category 1),
the proposed change, and its risk (does it touch hooks, unsafe, or anything that runs in the game?).
End with:

- **Deliberately not changing**: things you considered and decided are fine, with the reason, so the
  next review doesn't re-litigate them.
- **Proposed refactor plan**: an ordered list of small steps, each one concern, each independently
  buildable. Safety nets (tests) first, bug fixes next, then moves/splits, then simplification,
  then docs.

Then stop and ask the user which steps to do. If the scope was "review only", stop here.

## Phase 3: refactor (only the approved steps)

- Confirm you're on `main` with a clean tree before the first change.
- One step = one commit. Keep each behaviour-preserving unless it's an approved bug fix, and never
  mix a move with a change: move code verbatim in one commit, change it in the next, so the diff is
  reviewable.
- After each step: `cargo fmt --all`, `cargo clippy --workspace --all-targets --release`, `cargo
  test --release`. All clean before committing.
- Build (`cargo build --release`) only once a step is complete, since it's swapped into the running
  game. For steps that change what's drawn, hooking, unloading, or anything under `unsafe`, ask the
  user to check it in game (and to exercise a hot reload for anything touching unload) before
  committing.
- Don't build the host (`--workspace` build) unless the user confirms the game is closed.
- No new crates, no loosened pins, no `API_VERSION` bump unless the exports changed.
- If a step turns out bigger or riskier than planned, stop and report instead of improvising.
- Commit messages: a summary line, then a body explaining why, following `CLAUDE.md`.

Finish with a short summary: what was done (commits), what was skipped and why, and anything you
found along the way that wasn't in the plan.
