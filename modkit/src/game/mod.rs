//! Safe bindings to the running game, for features.
//!
//! Features get a [`Game`] (and, while rendering, a [`Frame`]) only while it's safe to use the
//! game's state: on the render thread inside the game's `render_game`, or while the game's other
//! threads are paused. The views borrow from the callback they're given to, so they can't be kept
//! or sent to another thread, and everything they offer is safe to call.
//!
//! This module only knows the game. Where things are comes from the game's symbols (`bindings`);
//! nothing here depends on how the mod is loaded or hooked.

pub(crate) mod bindings;
mod render;
pub(crate) mod symbols;
mod tooltip;
mod types;
mod world;

use std::marker::PhantomData;
use std::ptr;

pub use render::{Camera, Frame, Stage};
pub use tooltip::{PickupTooltip, WorldTooltip};
pub use types::{CircleRenderInfo, IconRenderInfo, LineRenderInfo, Real2, Real4x4, Wall};
pub use world::{Map, Material, Pickup, Pickups, PickupsId};

use bindings::Bindings;

/// The game's state, while it's safe to use.
#[derive(Clone, Copy)]
pub struct Game<'a> {
    bindings: &'a Bindings,
    /// Not `Send` or `Sync`: the game's state may only be used on the thread it was handed to.
    _not_send: PhantomData<*const ()>,
}

impl<'a> Game<'a> {
    /// # Safety
    ///
    /// For as long as the returned value is used, this thread must have exclusive use of the game's
    /// state: on the render thread inside `render_game`, or with the game's other threads paused.
    pub(crate) unsafe fn new(bindings: &'a Bindings) -> Self {
        Game {
            bindings,
            _not_send: PhantomData,
        }
    }

    /// The cell pickups lying around in the world.
    pub fn pickups(&self) -> Pickups<'a> {
        // SAFETY: `w.cell_pickups` and `w.n_cell_pickups`.
        unsafe {
            Pickups::new(
                *self,
                read(self.bindings.world + self.bindings.cell_pickups),
                read(self.bindings.world + self.bindings.n_cell_pickups),
            )
        }
    }

    /// The world map: which areas are explored, and the walls.
    pub fn map(&self) -> Map<'a> {
        Map::new(*self)
    }

    /// How far the map screen has faded in: 0 when closed, rising to 1 while it's open.
    pub fn map_fade(&self) -> f32 {
        // SAFETY: A static float in `render_game`.
        let fade = unsafe { read::<f32>(self.bindings.map_icon_alpha) };
        if fade.is_nan() { 0.0 } else { fade }
    }

    /// Whether the map screen is open (or still visibly fading out).
    pub fn map_open(&self) -> bool {
        self.map_fade() > 0.01
    }

    /// Whether the player has the map screen open: unlike `map_open`, false as soon as it starts
    /// closing.
    pub fn map_mode(&self) -> bool {
        let (offset, bit) = self.bindings.map_mode;
        // SAFETY: The `u32` storage of the `w.map_mode` bitfield.
        unsafe { read::<u32>(self.bindings.world + offset) & (1 << bit) != 0 }
    }

    /// The game's `w.frame_number`. Despite the name, it counts simulation steps, which run at a
    /// fixed 120 per second regardless of frame rate, so it's a good clock for animations.
    pub fn frame_number(&self) -> i32 {
        // SAFETY: `w.frame_number`, an int.
        unsafe { read(self.bindings.world + self.bindings.frame_number) }
    }
}

/// Reads a value of type `T` from game memory.
///
/// # Safety
///
/// `address` must be readable for `size_of::<T>()` bytes and hold a valid `T`.
unsafe fn read<T: Copy>(address: usize) -> T {
    // SAFETY: Guaranteed by the caller; unaligned reads are fine for any address.
    unsafe { ptr::read_unaligned(address as *const T) }
}

/// Writes a value of type `T` to game memory.
///
/// # Safety
///
/// `address` must be writable for `size_of::<T>()` bytes.
unsafe fn write<T: Copy>(address: usize, value: T) {
    // SAFETY: Guaranteed by the caller; unaligned writes are fine for any address.
    unsafe { ptr::write_unaligned(address as *mut T, value) }
}
