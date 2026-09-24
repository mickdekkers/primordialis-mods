//! Turns the game's rendering into feature calls, by hooking two game functions:
//!
//! - `render_game(world_rc, ui_rc, ...)`: renders a frame. Gives the world render context (the
//!   camera) for the frame, and marks its end.
//! - `begin_trace_stage(name)`: called at the start of every rendering stage (it only records timings
//!   when the profiler is on). Anchoring on stage names instead of code addresses survives game
//!   updates. A stage ends when the next begins, or when the frame ends.

use std::cell::Cell;
use std::ffi::{CStr, c_char, c_void};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::{Mutex, TryLockError};

use crate::Result;
use crate::feature::{Feature, Features};
use crate::game::bindings::Bindings;
use crate::game::{Frame, Game, Stage};
use crate::hook::{Hook, Hooks, InFlight, Original};
use crate::log;

type RenderGame = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void, f32, *mut c_void);
type BeginTraceStage = unsafe extern "C" fn(*const c_char);

static ORIGINAL_RENDER_GAME: Original<RenderGame> = Original::new();
static ORIGINAL_BEGIN_TRACE_STAGE: Original<BeginTraceStage> = Original::new();

struct Running {
    bindings: Bindings,
    features: Mutex<Features>,
    hooks: Hooks,
}

/// Set while hooks exist. The detours only run while it's set, and it's only cleared once no thread
/// can be in a detour anymore.
static RUNNING: AtomicPtr<Running> = AtomicPtr::new(ptr::null_mut());
static LOGGED_FIRST_FRAME: AtomicBool = AtomicBool::new(false);

/// The frame being rendered on this thread.
#[derive(Clone, Copy)]
struct FrameState {
    /// The world `render_context*` of the `render_game` call in progress, or 0.
    render_context: usize,
    /// The name of the current stage, or 0.
    stage: usize,
}

thread_local! {
    // No destructor, so nothing is left behind on the game's threads when the mod is unloaded.
    static FRAME: Cell<FrameState> = const { Cell::new(FrameState { render_context: 0, stage: 0 }) };
}

/// Hooks the game, and starts calling `features`. Makes no assumptions about what the game is doing:
/// until the next frame starts, features aren't called.
pub fn start(bindings: Bindings, features: Features) -> Result<()> {
    if !RUNNING.load(Ordering::Acquire).is_null() {
        return Err("already running".into());
    }
    // SAFETY: The addresses come from the game's own symbols, and the detours' signatures match the
    // game's declarations: `void render_game(render_context*, render_context*, user_input*,
    // recording_buffer*, float, window_t*)` and `void begin_trace_stage(char*)`.
    let hooks = unsafe {
        vec![
            Hook::new("render_game", bindings.render_game, render_game as RenderGame, &ORIGINAL_RENDER_GAME),
            Hook::new(
                "begin_trace_stage",
                bindings.begin_trace_stage,
                begin_trace_stage as BeginTraceStage,
                &ORIGINAL_BEGIN_TRACE_STAGE,
            ),
        ]
    };
    let hooks = Hooks::new(hooks)?;
    let running = Box::into_raw(Box::new(Running { bindings, features: Mutex::new(features), hooks }));
    RUNNING.store(running, Ordering::Release);
    // SAFETY: Valid until freed below or by `stop`.
    if let Err(error) = unsafe { &*running }.hooks.enable() {
        // Never enabled (or rolled back while paused), so no thread can be in a detour.
        RUNNING.store(ptr::null_mut(), Ordering::Release);
        // SAFETY: Allocated above, and nothing refers to it anymore.
        drop(unsafe { Box::from_raw(running) });
        return Err(error);
    }
    Ok(())
}

/// Undoes the features' changes and removes the hooks, once no thread is inside the mod. Afterwards,
/// none of the mod's code runs anymore (except its own threads). On failure, everything keeps
/// running.
pub fn stop() -> Result<()> {
    let running = RUNNING.load(Ordering::Acquire);
    if running.is_null() {
        return Ok(());
    }
    // SAFETY: Valid while hooks exist.
    let running_ref = unsafe { &*running };
    running_ref.hooks.disable(|| {
        // No thread is inside the mod, so none holds the lock.
        let mut features = running_ref.features.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // SAFETY: The game's other threads are paused.
        let game = unsafe { Game::new(&running_ref.bindings) };
        features.revert_all(|feature| feature.revert(&game));
    })?;
    RUNNING.store(ptr::null_mut(), Ordering::Release);
    // SAFETY: No thread is inside a detour or trampoline, and none can enter one anymore. Dropping
    // frees the features, and the trampolines and relays.
    drop(unsafe { Box::from_raw(running) });
    Ok(())
}

fn running() -> &'static Running {
    // SAFETY: Detours only run while hooks exist, and `stop` frees them only once no thread is inside
    // a detour (they count themselves with `InFlight` first).
    unsafe { &*RUNNING.load(Ordering::Acquire) }
}

extern "C" fn render_game(
    world_rc: *mut c_void,
    ui_rc: *mut c_void,
    input: *mut c_void,
    recording: *mut c_void,
    dt: f32,
    window: *mut c_void,
) {
    let _in_flight = InFlight::enter();
    let previous = FRAME.replace(FrameState { render_context: world_rc as usize, stage: 0 });
    // SAFETY: Forwards the game's own arguments to the original function.
    unsafe { ORIGINAL_RENDER_GAME.get()(world_rc, ui_rc, input, recording, dt, window) };
    let frame = FRAME.replace(previous);
    end_stage(frame);
}

extern "C" fn begin_trace_stage(name: *const c_char) {
    let _in_flight = InFlight::enter();
    let frame = FRAME.get();
    end_stage(frame);
    // SAFETY: Forwards the game's own argument to the original function.
    unsafe { ORIGINAL_BEGIN_TRACE_STAGE.get()(name) };
    // Outside `render_game` (or if the hooks were installed during this `render_game` call) there's
    // no frame.
    if frame.render_context == 0 {
        return;
    }
    FRAME.set(FrameState { stage: name as usize, ..frame });
    if !name.is_null() {
        // SAFETY: Stage names are NUL-terminated string literals.
        let stage = Stage::new(unsafe { CStr::from_ptr(name) });
        dispatch(frame.render_context, |feature, frame| feature.stage_begin(frame, stage));
    }
}

fn end_stage(frame: FrameState) {
    if frame.render_context != 0 && frame.stage != 0 {
        // SAFETY: A stage name `begin_trace_stage` was called with.
        let stage = Stage::new(unsafe { CStr::from_ptr(frame.stage as *const c_char) });
        dispatch(frame.render_context, |feature, frame| feature.stage_end(frame, stage));
    }
}

/// Calls every feature, on the render thread inside `render_game`.
fn dispatch(render_context: usize, mut event: impl FnMut(&mut dyn Feature, &Frame)) {
    let running = running();
    let mut features = match running.features.try_lock() {
        Ok(features) => features,
        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        // A feature called something that rendered a stage: don't call features re-entrantly.
        Err(TryLockError::WouldBlock) => return,
    };
    if !LOGGED_FIRST_FRAME.swap(true, Ordering::Relaxed) {
        log::info("hooks active: rendering reached the features");
    }
    // SAFETY: On the render thread inside `render_game`, with its world render context.
    let frame = unsafe { Frame::new(Game::new(&running.bindings), render_context) };
    features.each(|feature| event(feature, &frame), |feature| feature.revert(frame.game()));
}
