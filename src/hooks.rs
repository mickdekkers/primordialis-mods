//! Hooks two game functions:
//!
//! - `render_game(world_rc, ui_rc, ...)`: to know the world render context (camera) while a frame is
//!   being rendered.
//! - `begin_trace_stage(name)`: called at the start of every rendering stage (it only records timings
//!   when the profiler is on). When the "menus" stage starts, `render_game` has just drawn the other map
//!   markers into the UI framebuffer, above the fog of war, and is about to draw menus on top. That's
//!   where we draw. Anchoring on the stage name instead of a code address survives game updates.
//!   With `fix_echolocation_positions`, pickups are also moved out of walls during the "racing_overlay" stage
//!   before it, where the Echolocation mutation draws its markers.

use std::cell::Cell;
use std::ffi::{CStr, c_char, c_void};
use std::panic::{self, AssertUnwindSafe};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use detour::GenericDetour;

use crate::game::Layout;
use crate::{Result, config, log, overlay};

/// The rendering stage right before which the overlay is drawn.
const DRAW_STAGE: &CStr = c"menus";
/// The rendering stage in which the Echolocation mutation draws its markers (right before `DRAW_STAGE`).
const ECHOLOCATION_STAGE: &CStr = c"racing_overlay";

type RenderGame = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void, f32, *mut c_void);
type BeginTraceStage = unsafe extern "C" fn(*const c_char);

struct Hooks {
    layout: Layout,
    render_game: GenericDetour<RenderGame>,
    begin_trace_stage: GenericDetour<BeginTraceStage>,
}

static HOOKS: OnceLock<Hooks> = OnceLock::new();
/// Cleared if drawing ever panics, leaving the rest of the game running unmodified.
static OVERLAY_ENABLED: AtomicBool = AtomicBool::new(true);
static LOGGED_DRAW_STAGE: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// The world `render_context*` of the `render_game` call in progress on this thread, or 0.
    static WORLD_RC: Cell<usize> = const { Cell::new(0) };
}

pub fn install(layout: Layout) -> Result<()> {
    // SAFETY: The addresses come from the game's own symbols, and the signatures match the game's
    // declarations: `void render_game(render_context*, render_context*, user_input*, recording_buffer*,
    // float, window_t*)` and `void begin_trace_stage(char*)`.
    let hooks = unsafe {
        let render_game = std::mem::transmute::<usize, RenderGame>(layout.render_game);
        let begin_trace_stage = std::mem::transmute::<usize, BeginTraceStage>(layout.begin_trace_stage);
        Hooks {
            layout,
            render_game: GenericDetour::new(render_game, render_game_detour as extern "C" fn(_, _, _, _, _, _))
                .map_err(|e| format!("cannot hook render_game: {e}"))?,
            begin_trace_stage: GenericDetour::new(begin_trace_stage, begin_trace_stage_detour as extern "C" fn(_))
                .map_err(|e| format!("cannot hook begin_trace_stage: {e}"))?,
        }
    };
    let hooks = HOOKS.get_or_init(|| hooks);

    // SAFETY: We're called from DllMain during game startup, before the game has created other threads
    // or started rendering, so nothing can be executing the functions being patched.
    unsafe {
        hooks.begin_trace_stage.enable().map_err(|e| format!("cannot enable begin_trace_stage hook: {e}"))?;
        if let Err(e) = hooks.render_game.enable() {
            let _ = hooks.begin_trace_stage.disable();
            return Err(format!("cannot enable render_game hook: {e}"));
        }
    }
    Ok(())
}

fn hooks() -> &'static Hooks {
    // Detours are only enabled after HOOKS is set, so this can't fail inside a detour.
    HOOKS.get().expect("hooks are initialized before they are enabled")
}

extern "C" fn render_game_detour(
    world_rc: *mut c_void,
    ui_rc: *mut c_void,
    input: *mut c_void,
    recording: *mut c_void,
    dt: f32,
    window: *mut c_void,
) {
    let hooks = hooks();
    let previous = WORLD_RC.replace(world_rc as usize);
    // SAFETY: Forwards the game's own arguments to the original function.
    unsafe { hooks.render_game.call(world_rc, ui_rc, input, recording, dt, window) };
    WORLD_RC.set(previous);
    // Pickups moved for Echolocation are normally moved back at the start of DRAW_STAGE; this is in
    // case the game skipped it. SAFETY: Still on the render thread, and the game is done rendering.
    run_guarded(|| unsafe { overlay::restore_pickups(&hooks.layout) });
}

extern "C" fn begin_trace_stage_detour(name: *const c_char) {
    let hooks = hooks();
    // SAFETY: Forwards the game's own argument to the original function.
    unsafe { hooks.begin_trace_stage.call(name) };

    let world_rc = WORLD_RC.get();
    if world_rc == 0 || name.is_null() || !OVERLAY_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    // SAFETY: Stage names are NUL-terminated string literals.
    let name = unsafe { CStr::from_ptr(name) };
    if name == ECHOLOCATION_STAGE {
        if !config::current().fix_echolocation_positions {
            return;
        }
        // SAFETY: We're inside `render_game` on the render thread, where only the Echolocation
        // markers read pickups until DRAW_STAGE.
        run_guarded(|| unsafe { overlay::move_pickups_out_of_walls(&hooks.layout) });
        return;
    }
    if name != DRAW_STAGE {
        return;
    }
    if !LOGGED_DRAW_STAGE.swap(true, Ordering::Relaxed) {
        // This stage runs every frame, the main menu's background world included; `overlay::draw`
        // only draws while the map is open.
        log::info("hooks active: rendering reached the \"menus\" stage, where map icons are drawn");
    }

    // SAFETY: We're inside `render_game` on the render thread, with its world render context, at the
    // point where it draws UI-layer map markers.
    run_guarded(|| unsafe {
        overlay::restore_pickups(&hooks.layout);
        overlay::draw(&hooks.layout, world_rc, config::current().fix_icon_positions);
    });
}

/// Runs overlay code, disabling the overlay if it panics rather than unwinding into the game.
fn run_guarded(f: impl FnOnce()) {
    if panic::catch_unwind(AssertUnwindSafe(f)).is_err() {
        OVERLAY_ENABLED.store(false, Ordering::Relaxed);
        log::error("the map overlay panicked; it is disabled until the game restarts");
    }
}
