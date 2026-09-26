//! Turns the game's rendering into feature calls, by hooking game functions:
//!
//! - `render_game(world_rc, ui_rc, input, ..., dt, ...)`: renders a frame. Gives the frame's world and
//!   UI render contexts (their cameras and fonts), its input (the mouse) and its time step, and marks
//!   its end.
//! - `begin_trace_stage(name)`: called at the start of every rendering stage (it only records timings
//!   when the profiler is on). Anchoring on stage names instead of code addresses survives game
//!   updates. A stage ends when the next begins, or when the frame ends.
//! - `do_text_button(rc, input, pos, half_size, text)`: draws a button of the game's menus. Only
//!   hooked if the menu bindings resolved.

use std::cell::Cell;
use std::ffi::{CStr, c_char, c_void};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::{Mutex, TryLockError};

use crate::Result;
use crate::feature::{Feature, Features};
use crate::game::bindings::Bindings;
use crate::game::menu;
use crate::game::types::{Real2, Real3};
use crate::game::{Frame, Game, MenuButton, Stage};
use crate::hook::{Hook, Hooks, InFlight, Original};
use crate::log;

type RenderGame =
    unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void, f32, *mut c_void);
type BeginTraceStage = unsafe extern "C" fn(*const c_char);
/// `button do_text_button(render_context*, user_input*, real_3 pos, real_2 half_size, char* text)`.
/// The x64 ABI passes the 12-byte `real_3` by reference, and the 8-byte `real_2` by value in a
/// register. The result, whose bit 0 says whether the button was clicked, comes back in `eax`, which
/// the detour passes on whole.
type DoTextButton =
    unsafe extern "C" fn(*mut c_void, *mut c_void, *const Real3, Real2, *const c_char) -> u32;

static ORIGINAL_RENDER_GAME: Original<RenderGame> = Original::new();
static ORIGINAL_BEGIN_TRACE_STAGE: Original<BeginTraceStage> = Original::new();
static ORIGINAL_DO_TEXT_BUTTON: Original<DoTextButton> = Original::new();

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
    /// Its UI `render_context*` and `user_input*`.
    ui_render_context: usize,
    input: usize,
    /// Its `dt`: the time since the last frame, in seconds.
    dt: f32,
    /// The name of the current stage, or 0.
    stage: usize,
}

impl FrameState {
    const NONE: FrameState = FrameState {
        render_context: 0,
        ui_render_context: 0,
        input: 0,
        dt: 0.0,
        stage: 0,
    };
}

thread_local! {
    // No destructors, so nothing is left behind on the game's threads when the mod is unloaded.
    static FRAME: Cell<FrameState> = const { Cell::new(FrameState::NONE) };
    /// Set while features are called on this thread. A stage the game begins meanwhile (in a game
    /// function a feature called) is nested in the current one: it doesn't end the current stage,
    /// which would then never get its `stage_end`, and isn't reported itself.
    static DISPATCHING: Cell<bool> = const { Cell::new(false) };
}

/// Hooks the game, and starts calling `features`. Makes no assumptions about what the game is doing:
/// until the next frame starts, features aren't called.
pub fn start(bindings: Bindings, features: Features) -> Result<()> {
    if !RUNNING.load(Ordering::Acquire).is_null() {
        return Err("already running".into());
    }
    // SAFETY: The addresses come from the game's own symbols, and the detours' signatures match the
    // game's declarations: `void render_game(render_context*, render_context*, user_input*,
    // recording_buffer*, float, window_t*)`, `void begin_trace_stage(char*)` and `do_text_button`
    // (see `DoTextButton`).
    let mut hooks = unsafe {
        vec![
            Hook::new(
                "render_game",
                bindings.render_game,
                render_game as RenderGame,
                &ORIGINAL_RENDER_GAME,
            ),
            Hook::new(
                "begin_trace_stage",
                bindings.begin_trace_stage,
                begin_trace_stage as BeginTraceStage,
                &ORIGINAL_BEGIN_TRACE_STAGE,
            ),
        ]
    };
    if let Some(menu) = bindings.menu.ok() {
        // SAFETY: As above.
        hooks.push(unsafe {
            Hook::new(
                "do_text_button",
                menu.do_text_button,
                do_text_button as DoTextButton,
                &ORIGINAL_DO_TEXT_BUTTON,
            )
        });
    }
    let hooks = Hooks::new(hooks)?;
    let running = Box::into_raw(Box::new(Running {
        bindings,
        features: Mutex::new(features),
        hooks,
    }));
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
        let mut features = running_ref
            .features
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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
    // Both render contexts are always given; a frame without one isn't one features can draw in.
    let frame = if world_rc.is_null() || ui_rc.is_null() {
        FrameState::NONE
    } else {
        FrameState {
            render_context: world_rc as usize,
            ui_render_context: ui_rc as usize,
            input: input as usize,
            dt,
            stage: 0,
        }
    };
    let previous = FRAME.replace(frame);
    // SAFETY: Forwards the game's own arguments to the original function.
    unsafe { ORIGINAL_RENDER_GAME.get()(world_rc, ui_rc, input, recording, dt, window) };
    let frame = FRAME.replace(previous);
    end_stage(frame);
}

extern "C" fn begin_trace_stage(name: *const c_char) {
    let _in_flight = InFlight::enter();
    if DISPATCHING.get() {
        // SAFETY: Forwards the game's own argument to the original function.
        unsafe { ORIGINAL_BEGIN_TRACE_STAGE.get()(name) };
        return;
    }
    let frame = FRAME.get();
    end_stage(frame);
    // SAFETY: Forwards the game's own argument to the original function.
    unsafe { ORIGINAL_BEGIN_TRACE_STAGE.get()(name) };
    // Outside `render_game` (or if the hooks were installed during this `render_game` call) there's
    // no frame.
    if frame.render_context == 0 {
        return;
    }
    FRAME.set(FrameState {
        stage: name as usize,
        ..frame
    });
    if !name.is_null() {
        // SAFETY: Stage names are NUL-terminated string literals.
        let stage = Stage::new(unsafe { CStr::from_ptr(name) });
        dispatch(frame, |feature, frame| feature.stage_begin(frame, stage));
    }
}

extern "C" fn do_text_button(
    render_context: *mut c_void,
    input: *mut c_void,
    position: *const Real3,
    half_size: Real2,
    text: *const c_char,
) -> u32 {
    let _in_flight = InFlight::enter();
    let original = ORIGINAL_DO_TEXT_BUTTON.get();
    // Only hooked if the menu bindings resolved. A button drawn by something a feature called isn't
    // passed to the features again.
    let menu = running().bindings.menu.ok();
    let usable =
        !DISPATCHING.get() && !render_context.is_null() && !position.is_null() && !text.is_null();
    let Some(menu) = menu.filter(|_| usable) else {
        // SAFETY: Forwards the game's own arguments to the original function.
        return unsafe { original(render_context, input, position, half_size, text) };
    };
    // SAFETY: The game passes the button's position as a `real_3*`, and its text as a NUL-terminated
    // string, which outlives the call.
    let (at, label) = unsafe { (position.read_unaligned(), CStr::from_ptr(text)) };
    // SAFETY: Drawing the menu, with the render context the game draws the button with.
    let size = unsafe { menu::text_size(&menu, render_context as usize, label) };
    let mut button = MenuButton::new(&menu, label, size, at);
    dispatch_menu_button(&mut button);
    let moved = button.position();
    // SAFETY: The game's own arguments, but with the position the features chose: a by-value
    // `real_3`, which the callee gets its own copy of either way.
    let result = unsafe { original(render_context, input, &moved, half_size, text) };
    // SAFETY: The game just drew the button with this render context.
    unsafe { menu::draw_labels(&menu, render_context as usize, &button) };
    result
}

/// Lets every feature change a menu button before the game draws it.
fn dispatch_menu_button(button: &mut MenuButton) {
    let running = running();
    let mut features = match running.features.try_lock() {
        Ok(features) => features,
        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        Err(TryLockError::WouldBlock) => return,
    };
    // SAFETY: The main and pause menus are drawn by `do_pause_menu`, which only `render_game` calls:
    // this is the render thread inside `render_game`. It's only used to revert a feature that panics.
    let game = unsafe { Game::new(&running.bindings) };
    // `each` catches the features' panics, so this is always reset.
    DISPATCHING.set(true);
    features.each(
        |feature| feature.menu_button(button),
        |feature| feature.revert(&game),
    );
    DISPATCHING.set(false);
}

fn end_stage(frame: FrameState) {
    if frame.render_context != 0 && frame.stage != 0 {
        // SAFETY: A stage name `begin_trace_stage` was called with.
        let stage = Stage::new(unsafe { CStr::from_ptr(frame.stage as *const c_char) });
        dispatch(frame, |feature, frame| feature.stage_end(frame, stage));
    }
}

/// Calls every feature, on the render thread inside `render_game`.
fn dispatch(state: FrameState, mut event: impl FnMut(&mut dyn Feature, &Frame)) {
    let running = running();
    let mut features = match running.features.try_lock() {
        Ok(features) => features,
        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        // Only if a feature called something that rendered a frame: don't call features re-entrantly.
        Err(TryLockError::WouldBlock) => return,
    };
    if !LOGGED_FIRST_FRAME.swap(true, Ordering::Relaxed) {
        log::info("hooks active: rendering reached the features");
    }
    // SAFETY: On the render thread inside `render_game`, with its render contexts and input.
    let frame = unsafe {
        Frame::new(
            Game::new(&running.bindings),
            state.render_context,
            state.ui_render_context,
            state.input,
            state.dt,
        )
    };
    // `each` catches the features' panics, so this is always reset.
    DISPATCHING.set(true);
    features.each(
        |feature| event(feature, &frame),
        |feature| feature.revert(frame.game()),
    );
    DISPATCHING.set(false);
}
