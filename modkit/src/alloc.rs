//! The mod allocates from its own private heap instead of the process heap:
//!
//! - Installing and removing hooks allocates while the game's other threads are suspended. If one of
//!   them were suspended while holding the process heap's lock, that would deadlock the game. Only the
//!   mod's own code uses this heap, and hooks are only patched when no suspended thread is running it.
//! - When the hot reload host unloads the mod, destroying the heap frees everything the mod ever
//!   allocated, including anything a thread-local or library kept around.

use std::alloc::{GlobalAlloc, Layout};
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use windows_sys::Win32::System::Memory::{
    GetProcessHeap, HEAP_ZERO_MEMORY, HeapAlloc, HeapCreate, HeapDestroy, HeapFree, HeapReAlloc,
};

/// `HeapAlloc` returns memory aligned to 16 bytes on x86-64.
const HEAP_ALIGNMENT: usize = 16;

static HEAP: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
/// Set once the heap is destroyed during unloading: allocations then leak instead of crashing.
static DESTROYED: AtomicBool = AtomicBool::new(false);

pub struct PrivateHeap;

fn heap() -> *mut c_void {
    let heap = HEAP.load(Ordering::Acquire);
    if !heap.is_null() {
        return heap;
    }
    if DESTROYED.load(Ordering::Acquire) {
        // SAFETY: Always valid.
        return unsafe { GetProcessHeap() };
    }
    // SAFETY: A growable heap with default options.
    let created = unsafe { HeapCreate(0, 0, 0) };
    if created.is_null() {
        return ptr::null_mut();
    }
    match HEAP.compare_exchange(
        ptr::null_mut(),
        created,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => created,
        Err(existing) => {
            // Another thread created one first.
            // SAFETY: Nothing was allocated from `created`.
            unsafe { HeapDestroy(created) };
            existing
        }
    }
}

/// Destroys the heap, freeing everything allocated from it. Only for when the module is being
/// unloaded and none of its code runs anymore.
pub fn destroy() {
    DESTROYED.store(true, Ordering::Release);
    let heap = HEAP.swap(ptr::null_mut(), Ordering::AcqRel);
    if !heap.is_null() {
        // SAFETY: Nothing uses the heap anymore; later frees are skipped.
        unsafe { HeapDestroy(heap) };
    }
}

// SAFETY: Follows `GlobalAlloc`'s contract; over-aligned allocations store the original pointer
// right before the aligned block, like std's Windows allocator.
unsafe impl GlobalAlloc for PrivateHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { allocate(layout, 0) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        unsafe { allocate(layout, HEAP_ZERO_MEMORY) }
    }

    unsafe fn dealloc(&self, block: *mut u8, layout: Layout) {
        if DESTROYED.load(Ordering::Acquire) {
            return;
        }
        let original = if layout.align() <= HEAP_ALIGNMENT {
            block
        } else {
            // SAFETY: `allocate` stored the original pointer right before the aligned block.
            unsafe { block.cast::<*mut u8>().sub(1).read() }
        };
        // SAFETY: `original` was allocated from this heap.
        unsafe { HeapFree(heap(), 0, original.cast()) };
    }

    unsafe fn realloc(&self, block: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if layout.align() > HEAP_ALIGNMENT || DESTROYED.load(Ordering::Acquire) {
            // SAFETY: Forwarded from the caller.
            unsafe {
                let new_layout = Layout::from_size_align_unchecked(new_size, layout.align());
                let new_block = self.alloc(new_layout);
                if !new_block.is_null() {
                    ptr::copy_nonoverlapping(block, new_block, layout.size().min(new_size));
                    self.dealloc(block, layout);
                }
                return new_block;
            }
        }
        // SAFETY: `block` was allocated from this heap.
        unsafe { HeapReAlloc(heap(), 0, block.cast(), new_size).cast() }
    }
}

unsafe fn allocate(layout: Layout, flags: u32) -> *mut u8 {
    let heap = heap();
    if heap.is_null() {
        return ptr::null_mut();
    }
    if layout.align() <= HEAP_ALIGNMENT {
        // SAFETY: A valid heap handle.
        return unsafe { HeapAlloc(heap, flags, layout.size()).cast() };
    }
    // Over-allocate, align within the block, and store the original pointer right before the
    // aligned block (there are at least 16 bytes before it, since the alignment is at least 32).
    let Some(size) = layout.size().checked_add(layout.align()) else {
        return ptr::null_mut();
    };
    // SAFETY: A valid heap handle.
    let original: *mut u8 = unsafe { HeapAlloc(heap, flags, size).cast() };
    if original.is_null() {
        return original;
    }
    let offset = layout.align() - (original as usize & (layout.align() - 1));
    // SAFETY: `offset <= align`, so the aligned block and the header before it are in bounds.
    unsafe {
        let aligned = original.add(offset);
        aligned.cast::<*mut u8>().sub(1).write(original);
        aligned
    }
}
