//! Hooks two game functions:
//!
//! - `render_game(world_rc, ui_rc, ...)`: to know the world render context (camera) while a frame is
//!   being rendered.
//! - `begin_trace_stage(name)`: called at the start of every rendering stage (it only records timings
//!   when the profiler is on). When the "menus" stage starts, `render_game` has just drawn the other map
//!   markers into the UI framebuffer, above the fog of war, and is about to draw menus on top. That's
//!   where we draw. Anchoring on the stage name instead of a code address survives game updates.
//!   With `fix_echolocation_positions`, pickups are also moved out of walls during the "racing_overlay"
//!   stage before it, where the Echolocation mutation draws its markers.
//!
//! Hooks can be installed and removed while the game runs (the hot reload host does both): the
//! game's other threads are paused while the code is patched, and only when none of them is executing
//! code that is about to change. Removing the hooks also waits until no thread is inside the mod,
//! so the module can be unloaded afterwards.

use std::cell::Cell;
use std::ffi::{CStr, c_char, c_void};
use std::ops::Range;
use std::panic::{self, AssertUnwindSafe};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use detour::RawDetour;
use windows_sys::Win32::System::Diagnostics::Debug::IMAGE_NT_HEADERS64;
use windows_sys::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, VirtualQuery};
use windows_sys::Win32::System::SystemServices::IMAGE_DOS_HEADER;

use crate::game::Layout;
use crate::{Result, config, freeze, log, overlay};

/// The rendering stage right before which the overlay is drawn.
const DRAW_STAGE: &CStr = c"menus";
/// The rendering stage in which the Echolocation mutation draws its markers (right before `DRAW_STAGE`).
const ECHOLOCATION_STAGE: &CStr = c"racing_overlay";

/// How long to keep trying to find a moment when patching is safe.
const PATCH_TIMEOUT: Duration = Duration::from_secs(10);
/// Bytes at the start of a hooked function that patching may overwrite (5 for a relative jump, 14
/// for an absolute one), rounded up.
const PATCH_LENGTH: usize = 16;

type RenderGame = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void, f32, *mut c_void);
type BeginTraceStage = unsafe extern "C" fn(*const c_char);

struct Hooks {
    layout: Layout,
    render_game: RawDetour,
    begin_trace_stage: RawDetour,
    /// The trampolines, which call the original functions.
    original_render_game: RenderGame,
    original_begin_trace_stage: BeginTraceStage,
}

/// Set while hooks exist. The detours only run while it's set, and it's only cleared once no thread
/// can be in a detour anymore.
static HOOKS: AtomicPtr<Hooks> = AtomicPtr::new(ptr::null_mut());
/// Number of threads currently inside a detour.
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
/// Cleared if drawing ever panics, leaving the rest of the game running unmodified.
static OVERLAY_ENABLED: AtomicBool = AtomicBool::new(true);
static LOGGED_DRAW_STAGE: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// The world `render_context*` of the `render_game` call in progress on this thread, or 0.
    /// (No destructor, so nothing is left behind on the game's threads when the mod is unloaded.)
    static WORLD_RC: Cell<usize> = const { Cell::new(0) };
}

/// Counts a thread as inside a detour for as long as it lives.
struct InFlight;

impl InFlight {
    fn enter() -> Self {
        IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
        InFlight
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
    }
}

pub fn install(layout: Layout) -> Result<()> {
    if !HOOKS.load(Ordering::Acquire).is_null() {
        return Err("hooks are already installed".into());
    }
    ensure_unpatched(layout.render_game, "render_game")?;
    ensure_unpatched(layout.begin_trace_stage, "begin_trace_stage")?;

    let creating = Instant::now();
    // SAFETY: The addresses come from the game's own symbols, and the detours' signatures match the
    // game's declarations: `void render_game(render_context*, render_context*, user_input*,
    // recording_buffer*, float, window_t*)` and `void begin_trace_stage(char*)`.
    let hooks = unsafe {
        let render_game = RawDetour::new(layout.render_game as *const (), render_game_detour as *const ())
            .map_err(|e| format!("cannot hook render_game: {e}"))?;
        let begin_trace_stage =
            RawDetour::new(layout.begin_trace_stage as *const (), begin_trace_stage_detour as *const ())
                .map_err(|e| format!("cannot hook begin_trace_stage: {e}"))?;
        Hooks {
            layout,
            original_render_game: std::mem::transmute::<*const (), RenderGame>(render_game.trampoline()),
            original_begin_trace_stage: std::mem::transmute::<*const (), BeginTraceStage>(
                begin_trace_stage.trampoline(),
            ),
            render_game,
            begin_trace_stage,
        }
    };
    let created = creating.elapsed();
    let prologues = [prologue(layout.render_game), prologue(layout.begin_trace_stage)];
    let hooks = Box::into_raw(Box::new(hooks));
    HOOKS.store(hooks, Ordering::Release);

    // SAFETY: `hooks` stays valid until freed below or by `uninstall`.
    let hooks_ref = unsafe { &*hooks };
    let result = freeze::while_paused(PATCH_TIMEOUT, |paused| {
        // A thread in the middle of a prologue would resume into the middle of the patch.
        if paused.any_executing_in(&prologues) {
            return None;
        }
        // SAFETY: No other thread is running, or executing the code being patched.
        Some(unsafe { enable(hooks_ref) })
    });
    match result {
        Ok((Ok(()), stats)) => {
            log::info(&format!("hooks installed (creating them took {created:?}; {stats:?})"));
            Ok(())
        }
        Ok((Err(error), _)) | Err(error) => {
            // Never enabled (or rolled back while paused), so no thread can be in a detour.
            HOOKS.store(ptr::null_mut(), Ordering::Release);
            // SAFETY: Allocated above, and nothing refers to it anymore.
            drop(unsafe { Box::from_raw(hooks) });
            Err(error)
        }
    }
}

/// Removes the hooks, once no thread is inside the mod. Afterwards, none of the mod's code can run
/// anymore (except its own threads), so the module can be unloaded. On failure, the hooks stay
/// installed and working.
pub fn uninstall() -> Result<()> {
    let hooks = HOOKS.load(Ordering::Acquire);
    if hooks.is_null() {
        return Ok(());
    }
    // SAFETY: Valid while installed.
    let hooks_ref = unsafe { &*hooks };
    let layout = &hooks_ref.layout;
    // Code a paused thread must not be executing: the mod itself, its trampolines and relays, and
    // the patched prologues.
    let mut regions = vec![module_range()?, prologue(layout.render_game), prologue(layout.begin_trace_stage)];
    for (detour, target, detour_fn) in [
        (&hooks_ref.render_game, layout.render_game, render_game_detour as *const () as usize),
        (&hooks_ref.begin_trace_stage, layout.begin_trace_stage, begin_trace_stage_detour as *const () as usize),
    ] {
        regions.push(allocation_range(detour.trampoline() as usize)?);
        if let Some(relay) = jump_destination(target).filter(|&destination| destination != detour_fn) {
            regions.push(allocation_range(relay)?);
        }
    }

    let result = freeze::while_paused(PATCH_TIMEOUT, |paused| {
        if IN_FLIGHT.load(Ordering::SeqCst) != 0 || paused.any_executing_in(&regions) {
            return None;
        }
        // SAFETY: No other thread is running, or inside the mod or the code being patched.
        Some(unsafe {
            overlay::restore_pickups(layout);
            disable(hooks_ref)
        })
    });
    match result {
        Ok((Ok(()), stats)) => {
            log::info(&format!("hooks removed ({stats:?})"));
            HOOKS.store(ptr::null_mut(), Ordering::Release);
            // SAFETY: No thread is inside a detour or trampoline, and none can enter one anymore.
            // Dropping frees the trampolines and relays.
            drop(unsafe { Box::from_raw(hooks) });
            Ok(())
        }
        Ok((Err(error), _)) | Err(error) => Err(error),
    }
}

/// Enables both hooks, or neither.
///
/// # Safety
///
/// Other threads must be paused, and not executing the prologues. Runs while they're paused (see
/// `freeze`).
unsafe fn enable(hooks: &Hooks) -> Result<()> {
    unsafe {
        if hooks.begin_trace_stage.enable().is_err() {
            return Err("cannot enable the begin_trace_stage hook".into());
        }
        if hooks.render_game.enable().is_err() {
            let _ = hooks.begin_trace_stage.disable();
            return Err("cannot enable the render_game hook".into());
        }
    }
    Ok(())
}

/// Disables both hooks, or neither.
///
/// # Safety
///
/// As `enable`, and no thread may be inside a detour.
unsafe fn disable(hooks: &Hooks) -> Result<()> {
    unsafe {
        if hooks.render_game.disable().is_err() {
            return Err("cannot disable the render_game hook (was it patched again by something else?)".into());
        }
        if hooks.begin_trace_stage.disable().is_err() {
            let _ = hooks.render_game.enable();
            return Err("cannot disable the begin_trace_stage hook (was it patched again by something else?)".into());
        }
    }
    Ok(())
}

/// The part of a function that hooking overwrites. A thread at its very first instruction is fine:
/// it executes either the original instruction or the jump, both whole.
fn prologue(function: usize) -> Range<usize> {
    function + 1..function + PATCH_LENGTH
}

/// Refuses functions that already start with a jump, e.g. hooked by another copy of the mod that
/// couldn't be unloaded, or by another tool.
fn ensure_unpatched(function: usize, name: &str) -> Result<()> {
    match jump_destination(function) {
        Some(destination) => Err(format!("{name} is already hooked by something else (jumps to {destination:#x})")),
        None => Ok(()),
    }
}

/// Where the jump at `address` goes, if it starts with one of the jumps hooking libraries write.
fn jump_destination(address: usize) -> Option<usize> {
    // SAFETY: The start of a function in the game's executable, which is always readable.
    unsafe {
        let code = address as *const u8;
        match (*code, *code.add(1)) {
            // jmp rel32
            (0xE9, _) => {
                let offset = code.add(1).cast::<i32>().read_unaligned();
                Some((address + 5).wrapping_add_signed(offset as isize))
            }
            // jmp [rip + rel32]
            (0xFF, 0x25) => {
                let offset = code.add(2).cast::<i32>().read_unaligned();
                let slot = (address + 6).wrapping_add_signed(offset as isize);
                Some((slot as *const usize).read_unaligned())
            }
            _ => None,
        }
    }
}

/// The address range of this module.
fn module_range() -> Result<Range<usize>> {
    let base = crate::own_module()? as usize;
    // SAFETY: A loaded module starts with valid DOS and NT headers.
    let size = unsafe {
        let dos = &*(base as *const IMAGE_DOS_HEADER);
        let nt = &*((base + dos.e_lfanew as usize) as *const IMAGE_NT_HEADERS64);
        nt.OptionalHeader.SizeOfImage as usize
    };
    Ok(base..base + size)
}

/// The whole allocation (as made by `VirtualAlloc`) containing `address`.
fn allocation_range(address: usize) -> Result<Range<usize>> {
    let query = |address: usize| {
        // SAFETY: `VirtualQuery` accepts any address.
        unsafe {
            let mut info: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
            let written = VirtualQuery(address as *const c_void, &mut info, size_of::<MEMORY_BASIC_INFORMATION>());
            (written != 0).then_some(info)
        }
    };
    let info = query(address).ok_or_else(|| format!("cannot query memory at {address:#x}"))?;
    let base = info.AllocationBase as usize;
    let mut end = base;
    while let Some(region) = query(end).filter(|region| region.AllocationBase as usize == base) {
        end += region.RegionSize;
    }
    Ok(base..end.max(address + 1))
}

fn hooks() -> &'static Hooks {
    // SAFETY: Detours only run while hooks are installed, and `uninstall` frees them only once no
    // thread is inside a detour (callers count themselves in `IN_FLIGHT` first).
    unsafe { &*HOOKS.load(Ordering::Acquire) }
}

extern "C" fn render_game_detour(
    world_rc: *mut c_void,
    ui_rc: *mut c_void,
    input: *mut c_void,
    recording: *mut c_void,
    dt: f32,
    window: *mut c_void,
) {
    let _in_flight = InFlight::enter();
    let hooks = hooks();
    let previous = WORLD_RC.replace(world_rc as usize);
    // SAFETY: Forwards the game's own arguments to the original function.
    unsafe { (hooks.original_render_game)(world_rc, ui_rc, input, recording, dt, window) };
    WORLD_RC.set(previous);
    // Pickups moved for Echolocation are normally moved back at the start of DRAW_STAGE; this is in
    // case the game skipped it. SAFETY: Still on the render thread, and the game is done rendering.
    run_guarded(|| unsafe { overlay::restore_pickups(&hooks.layout) });
}

extern "C" fn begin_trace_stage_detour(name: *const c_char) {
    let _in_flight = InFlight::enter();
    let hooks = hooks();
    // SAFETY: Forwards the game's own argument to the original function.
    unsafe { (hooks.original_begin_trace_stage)(name) };

    // Hooks may have been installed while this thread was already inside `render_game`; then this is
    // 0 until the next frame.
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
