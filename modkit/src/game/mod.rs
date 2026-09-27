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
pub(crate) mod menu;
mod pdb;
mod render;
pub(crate) mod symbols;
mod tooltip;
pub(crate) mod types;
mod world;

use std::marker::PhantomData;
use std::ptr;

pub use menu::MenuButton;
pub use render::{Camera, Frame, Stage};
pub use tooltip::{PickupTooltip, WorldTooltip};
pub use types::{CircleRenderInfo, IconRenderInfo, LineRenderInfo, Real2, Real4x4};
pub use world::{Map, MapId, Material, Pickup, Pickups, PickupsId};

use bindings::Bindings;

/// `MENU_NONE` in the game's `MENU` enum: no menu is open. Copied by hand from the PDB, which lists
/// the enum's values (`render_game` shows the pause menu for `MENU_PAUSE`, 1).
const MENU_NONE: i32 = 0;

/// Below this `map_fade`, the map screen has faded out: nothing of it shows anymore.
const MAP_SHOWN_FADE: f32 = 0.01;

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

    /// The world map: its light, and the walls.
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
        self.map_fade() > MAP_SHOWN_FADE
    }

    /// Whether the player has the map screen open: unlike `map_open`, false as soon as it starts
    /// closing.
    pub fn map_mode(&self) -> bool {
        let (offset, bit) = self.bindings.map_mode;
        // SAFETY: The `u32` storage of the `w.map_mode` bitfield.
        unsafe { read::<u32>(self.bindings.world + offset) & (1 << bit) != 0 }
    }

    /// Whether one of the game's menus, such as the pause menu, is open over the world, and over
    /// the map screen if that's open too. False if this version of the game doesn't have `w.menu` as
    /// expected.
    pub fn menu_open(&self) -> bool {
        let Some(offset) = self.bindings.open_menu.ok() else {
            return false;
        };
        // SAFETY: `w.menu`, a `MENU` enum stored as an int.
        unsafe { read::<i32>(self.bindings.world + offset) != MENU_NONE }
    }

    /// Where the game's camera is centered in the world. It follows the player (not the map screen's
    /// view), and the map is explored around it.
    pub fn view_center(&self) -> Real2 {
        // SAFETY: `w.camera_pos`, a `real_2`.
        unsafe { read(self.bindings.world + self.bindings.camera_pos) }
    }

    /// How far around `view_center` the player can see, in world units: the map's fog of war is clear
    /// within 80% of it and closes in by 100% (`walls.glsl`). It's the player's body's, so it can
    /// change during a run.
    pub fn vision_radius(&self) -> f32 {
        // SAFETY: `w.vision_radius`, a float.
        unsafe { read(self.bindings.world + self.bindings.vision_radius) }
    }

    /// The world's seed, which a saved run keeps. Runs can share one.
    pub fn seed(&self) -> u32 {
        // SAFETY: `w.run.seed`, an unsigned int.
        unsafe { read(self.bindings.world + self.bindings.seed) }
    }

    /// When the current run was started, as a timestamp the game saves with it: with the seed, it
    /// tells runs apart across game sessions, even ones started from the same seed.
    pub fn run_started_at(&self) -> f64 {
        // SAFETY: `w.run.start_time`, a double.
        unsafe { read(self.bindings.world + self.bindings.run_start_time) }
    }

    /// Which of the game's saves the current run is kept in: a normal run and a sandbox each have
    /// their own. `None` before a run is started or loaded, or if this version of the game doesn't
    /// have it as expected.
    pub fn save_slot(&self) -> Option<SaveSlot> {
        let slots = self.bindings.save_slots.ok()?;
        // SAFETY: `saver.save_dir`, a `char*`, only compared with the folders it can point at.
        let dir: usize = unsafe { read(slots.saver + slots.save_dir) };
        if dir == slots.saver + slots.normal_save_dir {
            Some(SaveSlot::Normal)
        } else if dir == slots.saver + slots.sandbox_save_dir {
            Some(SaveSlot::Sandbox)
        } else {
            None
        }
    }

    /// The game's `w.frame_number`. Despite the name, it counts simulation steps, which run at a
    /// fixed 120 per second regardless of frame rate, so it's a good clock for animations.
    pub fn frame_number(&self) -> i32 {
        // SAFETY: `w.frame_number`, an int.
        unsafe { read(self.bindings.world + self.bindings.frame_number) }
    }
}

/// One of the game's saves. It keeps one run of each kind, separately.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveSlot {
    Normal,
    Sandbox,
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
