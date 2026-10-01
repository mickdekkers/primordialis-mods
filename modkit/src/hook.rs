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
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE,
    PAGE_EXECUTE_WRITECOPY, PAGE_GUARD, PAGE_READONLY, PAGE_READWRITE, PAGE_WRITECOPY,
    VirtualQuery,
};

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
        Original {
            address: AtomicUsize::new(0),
            _function: PhantomData,
        }
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
    pub unsafe fn new<F: Copy>(
        name: &'static str,
        target: usize,
        detour: F,
        original: &'static Original<F>,
    ) -> Self {
        assert_eq!(size_of::<F>(), size_of::<usize>(), "not a function pointer");
        // SAFETY: A function pointer, as guaranteed by the caller.
        let detour = unsafe { std::mem::transmute_copy::<F, usize>(&detour) };
        Hook {
            name,
            target,
            detour,
            original: &original.address,
        }
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
            if let Some(existing) = existing_hook(hook.target) {
                // E.g. another copy of the mod that couldn't be unloaded: the mod would run twice.
                if existing.by_modkit {
                    return Err(format!(
                        "{} is already hooked by a modkit mod ({})",
                        hook.name, existing.by
                    ));
                }
                // Another mod, e.g. one hooked through MinHook by the Nucleus mod loader. Hooking on
                // top of it keeps both: the trampoline starts with its jump, so calling the
                // original calls its detour, which calls the function.
                log::info(&format!(
                    "{} is already hooked by {}, hooking on top of it",
                    hook.name, existing.by
                ));
            }
            // SAFETY: Guaranteed by `Hook::new`.
            let detour =
                unsafe { RawDetour::new(hook.target as *const (), hook.detour as *const ()) }
                    .map_err(|e| format!("cannot hook {}: {e}", hook.name))?;
            hook.original
                .store(detour.trampoline() as usize, Ordering::Release);
            created.push((hook, detour));
        }
        log::info(&format!(
            "created {} hooks in {:?}",
            created.len(),
            creating.elapsed()
        ));
        Ok(Hooks { hooks: created })
    }

    /// Enables every hook, or none. Detours may run as soon as this starts.
    pub fn enable(&self) -> Result<()> {
        let prologues: Vec<Range<usize>> =
            self.hooks.iter().map(|(hook, _)| hook.prologue()).collect();
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
            if let Some(relay) =
                jump_destination(hook.target).filter(|&destination| destination != hook.detour)
            {
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
            unsafe {
                if enabled {
                    detour.enable()
                } else {
                    detour.disable()
                }
            }
            .is_ok()
        };
        for (done, (hook, detour)) in self.hooks.iter().enumerate() {
            if !toggle(detour, enabled) {
                for (_, detour) in self.hooks[..done].iter().rev() {
                    toggle(detour, !enabled);
                }
                return Err(if enabled {
                    format!("cannot enable the {} hook", hook.name)
                } else {
                    format!(
                        "cannot disable the {} hook (was it patched again by something else?)",
                        hook.name
                    )
                });
            }
        }
        Ok(())
    }
}

/// A hook something else already put on a function.
struct ExistingHook {
    /// The file name of the module its jumps lead to, or their address when that isn't in a module.
    by: String,
    /// Whether that module is a modkit mod.
    by_modkit: bool,
}

/// The hook already on the function at `target`, if it starts with a hooking library's jump.
fn existing_hook(target: usize) -> Option<ExistingHook> {
    let detour = follow_jumps(jump_destination(target)?);
    let hooker = module::containing(detour);
    Some(ExistingHook {
        by: hooker
            .and_then(|module| module::path(module).ok())
            .and_then(|path| Some(path.file_name()?.to_string_lossy().into_owned()))
            .unwrap_or_else(|| format!("code at {detour:#x}")),
        by_modkit: hooker
            .is_some_and(|module| module::exports(module, modkit_protocol::EXPORT_API_VERSION)),
    })
}

/// Where the jump at `address` goes, if it starts with one of the jumps hooking libraries write.
fn jump_destination(address: usize) -> Option<usize> {
    match read::<[u8; 2]>(address)? {
        // jmp rel32
        [0xE9, _] => {
            let offset = read::<i32>(address + 1)?;
            Some((address + 5).wrapping_add_signed(offset as isize))
        }
        // jmp [rip + rel32]
        [0xFF, 0x25] => {
            let offset = read::<i32>(address + 2)?;
            read::<usize>((address + 6).wrapping_add_signed(offset as isize))
        }
        _ => None,
    }
}

/// Where the jumps starting at `address` lead: hooking libraries often jump to a relay first, which
/// jumps to their detour.
fn follow_jumps(mut address: usize) -> usize {
    for _ in 0..4 {
        match jump_destination(address) {
            Some(destination) => address = destination,
            None => break,
        }
    }
    address
}

/// The value at `address`, if the memory there can be read. Memory other hooking libraries
/// allocated is checked before it's read, so that a mistaken jump can't crash the game.
fn read<T: Copy>(address: usize) -> Option<T> {
    const READABLE: u32 = PAGE_READONLY
        | PAGE_READWRITE
        | PAGE_WRITECOPY
        | PAGE_EXECUTE_READ
        | PAGE_EXECUTE_READWRITE
        | PAGE_EXECUTE_WRITECOPY;
    let end = address.checked_add(size_of::<T>())?;
    let mut at = address;
    while at < end {
        let info = query(at)?;
        if info.State != MEM_COMMIT
            || info.Protect & READABLE == 0
            || info.Protect & PAGE_GUARD != 0
        {
            return None;
        }
        at = info.BaseAddress as usize + info.RegionSize;
    }
    // SAFETY: Committed, readable memory.
    Some(unsafe { (address as *const T).read_unaligned() })
}

/// What `VirtualQuery` knows about the memory at `address`.
fn query(address: usize) -> Option<MEMORY_BASIC_INFORMATION> {
    // SAFETY: `VirtualQuery` accepts any address.
    unsafe {
        let mut info: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
        let written = VirtualQuery(
            address as *const c_void,
            &mut info,
            size_of::<MEMORY_BASIC_INFORMATION>(),
        );
        (written != 0).then_some(info)
    }
}

/// The whole allocation (as made by `VirtualAlloc`) containing `address`.
fn allocation_range(address: usize) -> Result<Range<usize>> {
    let info = query(address).ok_or_else(|| format!("cannot query memory at {address:#x}"))?;
    let base = info.AllocationBase as usize;
    let mut end = base;
    while let Some(region) = query(end).filter(|region| region.AllocationBase as usize == base) {
        end += region.RegionSize;
    }
    Ok(base..end.max(address + 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hint::black_box;

    use windows_sys::Win32::System::Memory::{
        MEM_RELEASE, MEM_RESERVE, PAGE_NOACCESS, VirtualAlloc, VirtualFree, VirtualProtect,
    };

    type Function = extern "C" fn(u64) -> u64;

    #[inline(never)]
    extern "C" fn target(x: u64) -> u64 {
        black_box(x).wrapping_mul(3) ^ 0x55
    }

    /// Where `other_detour` finds the original function, set by `RawDetour`.
    static OTHER_ORIGINAL: AtomicUsize = AtomicUsize::new(0);

    /// Another mod's detour: adds 1000 to the result.
    extern "C" fn other_detour(x: u64) -> u64 {
        // SAFETY: Set to the trampoline of a function of this type before the detour is enabled.
        let original: Function =
            unsafe { std::mem::transmute(OTHER_ORIGINAL.load(Ordering::SeqCst)) };
        original(x) + 1000
    }

    static ORIGINAL: Original<Function> = Original::new();

    /// Our detour: multiplies the result by 10.
    extern "C" fn our_detour(x: u64) -> u64 {
        let _in_flight = InFlight::enter();
        ORIGINAL.get()(x) * 10
    }

    /// A function hooked by another library first (here `detour` itself; MinHook's jumps are the
    /// same) can be hooked on top: both detours run, ours first, and unhooking restores its hook.
    #[test]
    fn hooks_on_top_of_another_hook() {
        let function: Function = black_box(target);
        let expected = target(7);
        // SAFETY: Both functions have the same signature.
        let other =
            unsafe { RawDetour::new(target as *const (), other_detour as *const ()) }.unwrap();
        OTHER_ORIGINAL.store(other.trampoline() as usize, Ordering::SeqCst);
        // SAFETY: No other thread calls `target`.
        unsafe { other.enable() }.unwrap();
        assert!(jump_destination(target as *const () as usize).is_some());
        assert_eq!(function(7), expected + 1000);

        // SAFETY: `our_detour` has the signature of `target`.
        let hook = unsafe {
            Hook::new(
                "target",
                target as *const () as usize,
                our_detour as Function,
                &ORIGINAL,
            )
        };
        let hooks = Hooks::new(vec![hook]).unwrap();
        // SAFETY: No other thread calls `target`, or is inside `our_detour`.
        unsafe { hooks.set_enabled(true) }.unwrap();
        assert_eq!(function(7), (expected + 1000) * 10);

        // SAFETY: As above.
        unsafe { hooks.set_enabled(false) }.unwrap();
        assert_eq!(function(7), expected + 1000);
        // SAFETY: As above.
        unsafe { other.disable() }.unwrap();
        assert_eq!(function(7), expected);
    }

    /// A jump to a relay, which jumps to the detour through an address stored next to it, the way
    /// MinHook hooks on x64, is followed to the detour.
    #[test]
    fn follows_jumps_through_a_relay() {
        let detour = 0x1234_5678_9abc_usize;
        let mut code = [0u8; 32];
        hook_through_relay(&mut code, detour);
        let base = code.as_ptr() as usize;
        assert_eq!(jump_destination(base), Some(base + 16));
        assert_eq!(follow_jumps(base), detour);
        // Not jumps: call [rip + 0], and nothing.
        code[8..10].copy_from_slice(&[0xFF, 0x15]);
        assert_eq!(jump_destination(base + 8), None);
        assert_eq!(jump_destination(base + 10), None);
        // Unreadable memory isn't read.
        assert_eq!(follow_jumps(0), 0);
    }

    /// Writes a hook to `detour` at the start of `code`, through a relay, the way MinHook hooks on
    /// x64: a jmp rel32 to the relay at 16, which is a jmp [rip + 0] followed by the detour's address.
    fn hook_through_relay(code: &mut [u8; 32], detour: usize) {
        code[0] = 0xE9;
        code[1..5].copy_from_slice(&(16 - 5i32).to_le_bytes());
        code[16..22].copy_from_slice(&[0xFF, 0x25, 0, 0, 0, 0]);
        code[22..30].copy_from_slice(&detour.to_le_bytes());
    }

    /// An existing hook is named after the module its detour is in, which isn't a modkit mod here.
    #[test]
    fn names_the_module_an_existing_hook_leads_to() {
        let mut code = [0u8; 32];
        assert!(existing_hook(code.as_ptr() as usize).is_none());

        hook_through_relay(&mut code, other_detour as *const () as usize);
        let existing = existing_hook(code.as_ptr() as usize).unwrap();
        let test_exe = module::path(std::ptr::null_mut()).unwrap();
        assert_eq!(existing.by, test_exe.file_name().unwrap().to_string_lossy());
        assert!(!existing.by_modkit);

        hook_through_relay(&mut code, 0x1234_5678_9abc);
        let existing = existing_hook(code.as_ptr() as usize).unwrap();
        assert_eq!(existing.by, "code at 0x123456789abc");
        assert!(!existing.by_modkit);
    }

    /// Only committed memory that allows reading is read, all of it.
    #[test]
    fn reads_only_readable_memory() {
        const PAGE: usize = 4096;
        // SAFETY: Reserves four pages, released below.
        let base = unsafe { VirtualAlloc(std::ptr::null(), 4 * PAGE, MEM_RESERVE, PAGE_NOACCESS) }
            as usize;
        assert_ne!(base, 0);
        let commit = |page: usize, protect: u32| {
            let address = (base + page * PAGE) as *const c_void;
            // SAFETY: Commits a page of the reservation.
            let committed = unsafe { VirtualAlloc(address, PAGE, MEM_COMMIT, protect) };
            assert!(!committed.is_null());
        };
        commit(0, PAGE_READWRITE);
        commit(1, PAGE_NOACCESS);
        commit(2, PAGE_READWRITE | PAGE_GUARD);
        // The last page stays reserved.

        for protect in [PAGE_READONLY, PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE] {
            // SAFETY: Only changes the protection of a page of the reservation.
            let changed = unsafe {
                let mut old = 0;
                VirtualProtect(base as *const c_void, PAGE, protect, &mut old)
            };
            assert_ne!(changed, 0);
            assert_eq!(read::<u8>(base), Some(0), "{protect:#x}");
        }
        assert_eq!(read::<u8>(base), Some(0));
        assert_eq!(read::<u32>(base + PAGE - 4), Some(0));
        // Runs into the next page.
        assert_eq!(read::<u32>(base + PAGE - 2), None);
        assert_eq!(read::<u8>(base + PAGE), None);
        assert_eq!(read::<u8>(base + 2 * PAGE), None);
        assert_eq!(read::<u8>(base + 3 * PAGE), None);
        // Wraps around the end of the address space.
        assert_eq!(read::<u32>(usize::MAX - 1), None);
        assert_eq!(read::<u8>(0), None);

        // SAFETY: Releases the reservation, which nothing uses anymore.
        unsafe { VirtualFree(base as *mut c_void, 0, MEM_RELEASE) };
    }

    /// The whole allocation is found from any address in it, whatever its pages' states.
    #[test]
    fn finds_whole_allocations() {
        const PAGE: usize = 4096;
        // SAFETY: Reserves four pages and commits the second, released below.
        let base = unsafe {
            let base = VirtualAlloc(std::ptr::null(), 4 * PAGE, MEM_RESERVE, PAGE_NOACCESS);
            assert!(!VirtualAlloc(base.byte_add(PAGE), PAGE, MEM_COMMIT, PAGE_READWRITE).is_null());
            base as usize
        };
        for address in [base, base + PAGE + 12, base + 4 * PAGE - 1] {
            assert_eq!(allocation_range(address), Ok(base..base + 4 * PAGE));
        }
        // SAFETY: Releases the reservation, which nothing uses anymore.
        unsafe { VirtualFree(base as *mut c_void, 0, MEM_RELEASE) };
        // Freed memory belongs to no allocation, but its range still holds the address.
        assert!(allocation_range(base + 5).unwrap().contains(&(base + 5)));
    }

    #[test]
    fn prologues_follow_the_first_instruction() {
        // SAFETY: Never enabled; `our_detour` has the signature of `target`.
        let hook = unsafe { Hook::new("target", 0x1_0000, our_detour as Function, &ORIGINAL) };
        assert_eq!(hook.prologue(), 0x1_0001..0x1_0000 + PATCH_LENGTH);
    }

    #[test]
    fn threads_leaving_detours_are_no_longer_counted() {
        // Other tests' detours may run meanwhile, but not a thousand at once.
        let entered: Vec<InFlight> = (0..1000).map(|_| InFlight::enter()).collect();
        assert!(IN_FLIGHT.load(Ordering::SeqCst) >= 1000);
        drop(entered);
        assert!(IN_FLIGHT.load(Ordering::SeqCst) < 1000);
    }
}
