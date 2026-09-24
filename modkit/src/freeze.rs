//! Pausing the game's other threads, to patch code that they might be executing.
//!
//! While threads are paused, the calling code must not take locks those threads could hold: no
//! logging, and no allocation other than from the mod's private heap (see `alloc`).

use std::ops::Range;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::Debug::{
    CONTEXT, CONTEXT_CONTROL_AMD64, GetThreadContext,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId, GetThreadId, OpenThread,
    ResumeThread, SuspendThread, THREAD_GET_CONTEXT, THREAD_QUERY_INFORMATION,
    THREAD_SUSPEND_RESUME,
};

use crate::Result;

#[link(name = "ntdll", kind = "raw-dylib")]
unsafe extern "system" {
    /// Iterates over a process's threads: opens the thread after `thread` (the first one if null).
    /// Undocumented, but part of Windows since Vista. Much faster than a Toolhelp snapshot, which
    /// lists every thread of every process.
    fn NtGetNextThread(
        process: HANDLE,
        thread: HANDLE,
        access: u32,
        attributes: u32,
        flags: u32,
        next: *mut HANDLE,
    ) -> i32;
}

const STATUS_NO_MORE_ENTRIES: i32 = 0x8000_001A_u32 as i32;
const THREAD_ACCESS: u32 = THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT | THREAD_QUERY_INFORMATION;

/// Instruction pointer recorded for a thread whose context couldn't be read: it is treated as being
/// everywhere, so the caller retries.
const UNKNOWN: usize = usize::MAX;

/// Set if `NtGetNextThread` doesn't work, to use Toolhelp instead.
static USE_TOOLHELP: AtomicBool = AtomicBool::new(false);

/// `GetThreadContext` requires a 16-byte aligned `CONTEXT`, but windows-sys declares it with the
/// alignment of its fields (8).
#[repr(C, align(16))]
struct AlignedContext(CONTEXT);

/// The other threads of the process, paused until this is dropped.
pub struct Paused {
    /// Each paused thread and its instruction pointer.
    threads: Vec<(HANDLE, usize)>,
}

impl Paused {
    /// Whether any paused thread is executing code in one of `ranges`.
    pub fn any_executing_in(&self, ranges: &[Range<usize>]) -> bool {
        self.threads
            .iter()
            .any(|&(_, ip)| ip == UNKNOWN || ranges.iter().any(|range| range.contains(&ip)))
    }

    /// Suspends `thread` and records where it is, taking ownership of the handle. Returns false if
    /// it can't be suspended (typically because it's exiting), after closing the handle.
    ///
    /// # Safety
    ///
    /// `thread` must be a thread handle with `THREAD_ACCESS`, and `self.threads` must have spare
    /// capacity: pushing must not allocate while threads are paused.
    unsafe fn add(&mut self, thread: HANDLE) -> bool {
        debug_assert!(self.threads.len() < self.threads.capacity());
        // SAFETY: Guaranteed by the caller.
        unsafe {
            if SuspendThread(thread) == u32::MAX {
                CloseHandle(thread);
                return false;
            }
            // Also waits until the thread is actually suspended.
            let mut context: AlignedContext = std::mem::zeroed();
            context.0.ContextFlags = CONTEXT_CONTROL_AMD64;
            let ip = if GetThreadContext(thread, &mut context.0) != 0 {
                context.0.Rip as usize
            } else {
                UNKNOWN
            };
            self.threads.push((thread, ip));
        }
        true
    }
}

impl Drop for Paused {
    fn drop(&mut self) {
        for &(thread, _) in &self.threads {
            // SAFETY: Handles we opened and suspended.
            unsafe {
                ResumeThread(thread);
                CloseHandle(thread);
            }
        }
    }
}

enum Pausing {
    Done(Paused),
    /// More threads than there was room reserved for; nothing is paused.
    Full,
    /// A thread couldn't be paused (e.g. it's exiting); nothing is paused.
    Busy,
    /// `NtGetNextThread` doesn't work here; nothing is paused.
    Unsupported,
}

/// Pauses the other threads, each as soon as it's found. A thread created meanwhile is found too,
/// since the iteration ends with the newest threads.
fn pause_by_iterating(capacity: usize) -> Pausing {
    let mut paused = Paused {
        threads: Vec::with_capacity(capacity),
    };
    // SAFETY: Thread handles from `NtGetNextThread` with `THREAD_ACCESS`; each is either owned by
    // `paused` or closed.
    unsafe {
        let current = GetCurrentThreadId();
        let mut cursor: HANDLE = ptr::null_mut();
        // The current thread's handle, closed once the iteration has moved past it.
        let mut own: HANDLE = ptr::null_mut();
        let failure = loop {
            let mut next = ptr::null_mut();
            let status =
                NtGetNextThread(GetCurrentProcess(), cursor, THREAD_ACCESS, 0, 0, &mut next);
            if !own.is_null() {
                CloseHandle(own);
                own = ptr::null_mut();
            }
            if status == STATUS_NO_MORE_ENTRIES {
                break None;
            }
            if status < 0 {
                break Some(Pausing::Unsupported);
            }
            cursor = next;
            if GetThreadId(next) == current {
                own = next;
                continue;
            }
            if paused.threads.len() == paused.threads.capacity() {
                CloseHandle(next);
                break Some(Pausing::Full);
            }
            if !paused.add(next) {
                break Some(Pausing::Busy);
            }
        };
        // On failure, dropping `paused` resumes the threads paused so far.
        failure.unwrap_or(Pausing::Done(paused))
    }
}

/// Fallback: lists the other threads with a Toolhelp snapshot, then pauses them. A thread created in
/// between isn't paused; it would have to reach the patched code within milliseconds of starting.
fn pause_with_toolhelp() -> Result<Paused> {
    let ids = other_thread_ids()?;
    let mut paused = Paused {
        threads: Vec::with_capacity(ids.len()),
    };
    for id in ids {
        // SAFETY: Opens a thread by ID; `add` takes ownership of the handle.
        unsafe {
            let thread = OpenThread(THREAD_ACCESS, 0, id);
            if !thread.is_null() {
                paused.add(thread);
            }
        }
    }
    Ok(paused)
}

fn other_thread_ids() -> Result<Vec<u32>> {
    // SAFETY: Toolhelp calls on a snapshot handle we own.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err("cannot list the game's threads".into());
        }
        let (process, current) = (GetCurrentProcessId(), GetCurrentThreadId());
        let mut ids = Vec::new();
        let mut entry: THREADENTRY32 = std::mem::zeroed();
        entry.dwSize = size_of::<THREADENTRY32>() as u32;
        let mut more = Thread32First(snapshot, &mut entry) != 0;
        while more {
            if entry.th32OwnerProcessID == process && entry.th32ThreadID != current {
                ids.push(entry.th32ThreadID);
            }
            more = Thread32Next(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
        Ok(ids)
    }
}

/// Pauses all other threads, or returns `None` if one of them can't be paused right now.
fn pause_other_threads(capacity: &mut usize) -> Result<Option<Paused>> {
    loop {
        if USE_TOOLHELP.load(Ordering::Relaxed) {
            return pause_with_toolhelp().map(Some);
        }
        match pause_by_iterating(*capacity) {
            Pausing::Done(paused) => return Ok(Some(paused)),
            Pausing::Full => *capacity *= 2,
            Pausing::Busy => return Ok(None),
            Pausing::Unsupported => USE_TOOLHELP.store(true, Ordering::Relaxed),
        }
    }
}

/// How `while_paused` went.
#[derive(Debug)]
pub struct Stats {
    pub attempts: u32,
    /// The longest time the other threads were paused in one attempt.
    pub longest_pause: Duration,
}

/// Repeatedly pauses the other threads and calls `attempt` until it returns `Some`, resuming the
/// threads in between so the game keeps running. Fails after `timeout`.
///
/// `attempt` runs while the threads are paused: see the module documentation for what it must not do.
pub fn while_paused<T>(
    timeout: Duration,
    mut attempt: impl FnMut(&Paused) -> Option<T>,
) -> Result<(T, Stats)> {
    let deadline = Instant::now() + timeout;
    let mut stats = Stats {
        attempts: 0,
        longest_pause: Duration::ZERO,
    };
    let mut capacity = 256;
    loop {
        let started = Instant::now();
        // The threads are resumed as soon as `attempt` returns.
        let result = pause_other_threads(&mut capacity)?.and_then(|paused| attempt(&paused));
        stats.attempts += 1;
        stats.longest_pause = stats.longest_pause.max(started.elapsed());
        if let Some(result) = result {
            return Ok((result, stats));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the game's threads didn't reach a safe point within {timeout:?} ({} attempts)",
                stats.attempts
            ));
        }
        thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hint::black_box;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

    use detour::RawDetour;

    #[inline(never)]
    extern "C" fn target(x: u64) -> u64 {
        black_box(x).wrapping_mul(3) ^ 0x55
    }

    #[inline(never)]
    extern "C" fn replacement(x: u64) -> u64 {
        black_box(x).wrapping_mul(5) ^ 0xAA
    }

    /// Hooks and unhooks a function hundreds of times while other threads call it nonstop, patching
    /// only while they're paused outside its prologue. Any torn patch would crash or return garbage.
    /// Pauses every other thread of the test process, so run it alone:
    /// `cargo test --release -- --ignored --test-threads=1 patches_code_other_threads_are_running`
    #[test]
    #[ignore = "pauses all other threads of the test process; run alone with --test-threads=1"]
    fn patches_code_other_threads_are_running() {
        let stop = Arc::new(AtomicBool::new(false));
        let (original, replaced) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let workers: Vec<_> = (0..4)
            .map(|_| {
                let (stop, original, replaced) = (stop.clone(), original.clone(), replaced.clone());
                thread::spawn(move || {
                    let function: extern "C" fn(u64) -> u64 = black_box(target);
                    let mut x = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        x = x.wrapping_add(1);
                        let result = function(x);
                        if result == x.wrapping_mul(3) ^ 0x55 {
                            original.fetch_add(1, Ordering::Relaxed);
                        } else if result == x.wrapping_mul(5) ^ 0xAA {
                            replaced.fetch_add(1, Ordering::Relaxed);
                        } else {
                            panic!("garbage result {result:#x} for {x}");
                        }
                    }
                })
            })
            .collect();

        // SAFETY: Both functions have the same signature.
        let detour =
            unsafe { RawDetour::new(target as *const (), replacement as *const ()) }.unwrap();
        let start = target as *const () as usize;
        let prologue = start + 1..start + 16;
        let retries = AtomicUsize::new(0);
        let mut longest = Duration::ZERO;
        for round in 0..200 {
            let enable = round % 2 == 0;
            let toggled = while_paused(Duration::from_secs(5), |paused| {
                if paused.any_executing_in(std::slice::from_ref(&prologue)) {
                    retries.fetch_add(1, Ordering::Relaxed);
                    return None;
                }
                // SAFETY: No other thread runs, or executes the prologue.
                Some(
                    unsafe {
                        if enable {
                            detour.enable()
                        } else {
                            detour.disable()
                        }
                    }
                    .is_ok(),
                )
            });
            let (toggled, stats) = toggled.unwrap();
            assert!(toggled);
            longest = longest.max(stats.longest_pause);
            thread::sleep(Duration::from_micros(200));
        }
        stop.store(true, Ordering::Relaxed);
        for worker in workers {
            worker.join().unwrap();
        }
        println!(
            "{} original and {} replaced calls, {} retries, threads paused at most {longest:?}",
            original.load(Ordering::Relaxed),
            replaced.load(Ordering::Relaxed),
            retries.load(Ordering::Relaxed)
        );
        assert!(original.load(Ordering::Relaxed) > 0 && replaced.load(Ordering::Relaxed) > 0);
    }
}
