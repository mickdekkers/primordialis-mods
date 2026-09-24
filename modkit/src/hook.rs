//! Hooks functions of the running game, and unhooks them so the mod can be unloaded.
//!
//! Hooks are installed and removed while the game runs: its other threads are paused while code is
//! patched, and only when none of them is executing code that is about to change. Removing hooks
//! also waits until no thread is inside the mod, so that the module can be unloaded afterwards.
//!
//! A detour counts itself as running with [`InFlight::enter`], and calls the original function
//! through the [`Original`] it was registered with.

use std::ffi::c_void;
use std::marker::PhantomData;
use std::ops::Range;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use detour::RawDetour;
use windows_sys::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, VirtualQuery};

use crate::{Result, freeze, log, module};

/// How long to keep trying to find a moment when patching is safe.
const PATCH_TIMEOUT: Duration = Duration::from_secs(10);
/// Bytes at the start of a hooked function that patching may overwrite (5 for a relative jump, 14
/// for an absolute one), rounded up.
const PATCH_LENGTH: usize = 16;

/// Number of threads currently inside a detour.
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// Counts a thread as inside a detour for as long as it lives. Every detour starts with one.
pub struct InFlight(());

impl InFlight {
    pub fn enter() -> Self {
        IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
        InFlight(())
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Where a detour finds the original function: a trampoline to it, set when the hook is created.
pub struct Original<F> {
    address: AtomicUsize,
    _function: PhantomData<F>,
}

impl<F: Copy> Original<F> {
    pub const fn new() -> Self {
        Original { address: AtomicUsize::new(0), _function: PhantomData }
    }

    /// The original function. Only for detours, which only run while their hook exists.
    pub fn get(&self) -> F {
        let address = self.address.load(Ordering::Acquire);
        debug_assert!(address != 0, "hook not created");
        // SAFETY: `Hook::new` guarantees `F` is a function pointer type, and set the address.
        unsafe { std::mem::transmute_copy(&address) }
    }
}

/// A function to hook.
pub struct Hook {
    name: &'static str,
    target: usize,
    detour: usize,
    original: &'static AtomicUsize,
}

impl Hook {
    /// # Safety
    ///
    /// `F` must be a function pointer type matching the signature and calling convention of the
    /// function at `target`, which must be a function of the game.
    pub unsafe fn new<F: Copy>(name: &'static str, target: usize, detour: F, original: &'static Original<F>) -> Self {
        assert_eq!(size_of::<F>(), size_of::<usize>(), "not a function pointer");
        // SAFETY: A function pointer, as guaranteed by the caller.
        let detour = unsafe { std::mem::transmute_copy::<F, usize>(&detour) };
        Hook { name, target, detour, original: &original.address }
    }

    /// The part of the function that hooking overwrites. A thread at its very first instruction is
    /// fine: it executes either the original instruction or the jump, both whole.
    fn prologue(&self) -> Range<usize> {
        self.target + 1..self.target + PATCH_LENGTH
    }
}

/// Hooks created together, enabled and disabled all at once.
pub struct Hooks {
    hooks: Vec<(Hook, RawDetour)>,
}

impl Hooks {
    /// Creates the hooks (and their trampolines), without enabling them yet.
    pub fn new(hooks: Vec<Hook>) -> Result<Self> {
        let creating = Instant::now();
        let mut created = Vec::with_capacity(hooks.len());
        for hook in hooks {
            if let Some(destination) = jump_destination(hook.target) {
                // E.g. hooked by another copy of the mod that couldn't be unloaded, or another tool.
                return Err(format!("{} is already hooked by something else (jumps to {destination:#x})", hook.name));
            }
            // SAFETY: Guaranteed by `Hook::new`.
            let detour = unsafe { RawDetour::new(hook.target as *const (), hook.detour as *const ()) }
                .map_err(|e| format!("cannot hook {}: {e}", hook.name))?;
            hook.original.store(detour.trampoline() as usize, Ordering::Release);
            created.push((hook, detour));
        }
        log::info(&format!("created {} hooks in {:?}", created.len(), creating.elapsed()));
        Ok(Hooks { hooks: created })
    }

    /// Enables every hook, or none. Detours may run as soon as this starts.
    pub fn enable(&self) -> Result<()> {
        let prologues: Vec<Range<usize>> = self.hooks.iter().map(|(hook, _)| hook.prologue()).collect();
        let (result, stats) = freeze::while_paused(PATCH_TIMEOUT, |paused| {
            // A thread in the middle of a prologue would resume into the middle of the patch.
            if paused.any_executing_in(&prologues) {
                return None;
            }
            // SAFETY: No other thread is running, or executing the code being patched.
            Some(unsafe { self.set_enabled(true) })
        })?;
        result?;
        log::info(&format!("hooks enabled ({stats:?})"));
        Ok(())
    }

    /// Disables every hook once no thread is inside the mod, or none. Just before, while the game's
    /// other threads are paused and none of them is inside the mod, calls `undo`, to undo changes
    /// the mod made to the game. Afterwards, none of the mod's code runs anymore (except its own
    /// threads), so the module can be unloaded.
    pub fn disable(&self, mut undo: impl FnMut()) -> Result<()> {
        // Code a paused thread must not be executing: the mod itself, its trampolines and relays, and
        // the patched prologues.
        let mut regions = vec![module::own_range()?];
        for (hook, detour) in &self.hooks {
            regions.push(hook.prologue());
            regions.push(allocation_range(detour.trampoline() as usize)?);
            if let Some(relay) = jump_destination(hook.target).filter(|&destination| destination != hook.detour) {
                regions.push(allocation_range(relay)?);
            }
        }
        let (result, stats) = freeze::while_paused(PATCH_TIMEOUT, |paused| {
            if IN_FLIGHT.load(Ordering::SeqCst) != 0 || paused.any_executing_in(&regions) {
                return None;
            }
            undo();
            // SAFETY: No other thread is running, or inside the mod or the code being patched.
            Some(unsafe { self.set_enabled(false) })
        })?;
        result?;
        log::info(&format!("hooks disabled ({stats:?})"));
        Ok(())
    }

    /// Enables or disables every hook, or rolls back and fails.
    ///
    /// # Safety
    ///
    /// Other threads must be paused, and not executing the prologues. When disabling, no thread may
    /// be inside a detour. Runs while threads are paused (see `freeze`).
    unsafe fn set_enabled(&self, enabled: bool) -> Result<()> {
        let toggle = |detour: &RawDetour, enabled: bool| {
            // SAFETY: Guaranteed by the caller.
            unsafe { if enabled { detour.enable() } else { detour.disable() } }.is_ok()
        };
        for (done, (hook, detour)) in self.hooks.iter().enumerate() {
            if !toggle(detour, enabled) {
                for (_, detour) in self.hooks[..done].iter().rev() {
                    toggle(detour, !enabled);
                }
                return Err(if enabled {
                    format!("cannot enable the {} hook", hook.name)
                } else {
                    format!("cannot disable the {} hook (was it patched again by something else?)", hook.name)
                });
            }
        }
        Ok(())
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
