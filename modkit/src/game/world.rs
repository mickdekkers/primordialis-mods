//! The world: cell pickups, their materials, and the map.

use std::ffi::c_void;

use super::types::{Real2, Wall, WallSample};
use super::{Game, read, write};

/// `wall_t wall_map(map_t*, real_2, bool)`. The 24-byte `wall_t` is returned through a hidden pointer
/// argument, which Rust does as well for this signature.
type WallMap = unsafe extern "C" fn(*const c_void, Real2, bool) -> WallSample;
/// `float light_value(map_t*, real_2)`: blends `map.light` of the hexes around the position.
type LightValue = unsafe extern "C" fn(*const c_void, Real2) -> f32;

/// The game's array of cell pickups (`w.cell_pickups`).
#[derive(Clone, Copy)]
pub struct Pickups<'a> {
    game: Game<'a>,
    address: usize,
    len: usize,
}

/// The pickup array's address and length. When either changes, pickups were added or removed, and an
/// index may no longer refer to the same pickup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PickupsId {
    address: usize,
    len: usize,
}

impl PickupsId {
    /// An id that no pickup array has to have, for tests. Ids are only ever compared, so any value
    /// is harmless.
    #[doc(hidden)]
    pub const fn for_tests(address: usize, len: usize) -> Self {
        PickupsId { address, len }
    }
}

impl<'a> Pickups<'a> {
    pub(super) fn new(game: Game<'a>, address: usize, count: i32) -> Self {
        let len = if address == 0 {
            0
        } else {
            count.max(0) as usize
        };
        Pickups { game, address, len }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn id(&self) -> PickupsId {
        PickupsId {
            address: self.address,
            len: self.len,
        }
    }

    pub fn get(&self, index: usize) -> Option<Pickup<'a>> {
        (index < self.len).then(|| Pickup {
            game: self.game,
            address: self.address + index * self.game.bindings.pickup_size,
        })
    }

    pub fn iter(&self) -> impl Iterator<Item = Pickup<'a>> + use<'a> {
        let pickups = *self;
        (0..self.len).filter_map(move |index| pickups.get(index))
    }
}

/// A cell pickup (`cell_pickup`).
#[derive(Clone, Copy)]
pub struct Pickup<'a> {
    game: Game<'a>,
    address: usize,
}

impl<'a> Pickup<'a> {
    /// Center, in world units.
    pub fn position(&self) -> Real2 {
        // SAFETY: `cell_pickup.x`, a `real_2` inside the pickup array.
        unsafe { read(self.address + self.game.bindings.pickup_x) }
    }

    /// Moves the pickup. The game's physics moves it on from there (when it's near the camera).
    pub fn set_position(&self, position: Real2) {
        // SAFETY: As in `position`; any position is valid, even an unreachable one.
        unsafe { write(self.address + self.game.bindings.pickup_x, position) }
    }

    /// Opacity, from 0 to 1. The game draws pickups in the world with it.
    pub fn alpha(&self) -> f32 {
        // SAFETY: `cell_pickup.alpha`, a float.
        unsafe { read(self.address + self.game.bindings.pickup_alpha) }
    }

    /// Changes the pickup's opacity.
    pub fn set_alpha(&self, alpha: f32) {
        // SAFETY: As in `alpha`; any float is valid.
        unsafe { write(self.address + self.game.bindings.pickup_alpha, alpha) }
    }

    /// Radius, in world units.
    pub fn radius(&self) -> f32 {
        // SAFETY: `cell_pickup.r`, a float.
        unsafe { read(self.address + self.game.bindings.pickup_r) }
    }

    /// The cell this pickup gives, if its material index is valid.
    pub fn material(&self) -> Option<Material<'a>> {
        Material::get(self.game, self.material_index())
    }

    /// Index of the cell this pickup gives in the game's materials, which may be invalid.
    pub(super) fn material_index(&self) -> i32 {
        // SAFETY: `cell_pickup.material_index`, an int.
        unsafe { read(self.address + self.game.bindings.pickup_material_index) }
    }

    /// Whether this is part of a combo cell: those are drawn in shifting colors, with a ring of
    /// particles around them.
    pub fn is_combo(&self) -> bool {
        let (offset, bit) = self.game.bindings.pickup_is_combo;
        // SAFETY: The `u32` storage of the `cell_pickup.is_combo` bitfield.
        unsafe { read::<u32>(self.address + offset) & (1 << bit) != 0 }
    }
}

/// A kind of cell (`material_t`).
#[derive(Clone, Copy)]
pub struct Material<'a> {
    game: Game<'a>,
    address: usize,
}

impl<'a> Material<'a> {
    /// The material at `index` in the game's `materials_list`, if there is one.
    pub(super) fn get(game: Game<'a>, index: i32) -> Option<Self> {
        let bindings = game.bindings;
        // SAFETY: The globals `materials_list` and `n_materials`.
        unsafe {
            let materials = read::<usize>(bindings.materials_list);
            let count = read::<i32>(bindings.n_materials);
            (materials != 0 && (0..count).contains(&index)).then(|| Material {
                game,
                address: materials + index as usize * bindings.material_size,
            })
        }
    }

    /// Identifies the material in any game session, unlike its index: combo materials are added to
    /// the list as they're made.
    pub fn id(&self) -> u32 {
        // SAFETY: `material_t.id`, an unsigned int.
        unsafe { read(self.address + self.game.bindings.material_id) }
    }

    /// How much of a body's genome size the cell takes up.
    pub fn genome_size(&self) -> f32 {
        // SAFETY: `material_t.genome_size`, a float.
        unsafe { read(self.address + self.game.bindings.material_genome_size) }
    }

    /// RGBA.
    pub fn base_color(&self) -> [f32; 4] {
        // SAFETY: `material_t.base_color`, a `real_4`.
        unsafe { read(self.address + self.game.bindings.material_base_color) }
    }

    /// Its icon, for `IconRenderInfo::uv`.
    pub fn icon_uv(&self) -> [f32; 2] {
        // SAFETY: `material_t.uv`, a `real_2`.
        unsafe { read(self.address + self.game.bindings.material_uv) }
    }
}

/// Identifies a map: it changes when another world is loaded or generated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapId {
    explored: usize,
    lower: (i32, i32),
    upper: (i32, i32),
}

/// The world map (`w.map`): a grid of hexes, their light, and the walls.
#[derive(Clone, Copy)]
pub struct Map<'a> {
    game: Game<'a>,
    /// The hex grid's bounds and its `explored` array, which only identify the map (`MapId`).
    lower: (i32, i32),
    upper: (i32, i32),
    explored: usize,
}

impl<'a> Map<'a> {
    pub(super) fn new(game: Game<'a>) -> Self {
        let (world, bindings) = (game.bindings.world, game.bindings);
        let range = bindings.map_range;
        // SAFETY: The `int`s of `w.map.map_range`, and the `float*` `w.map.explored`.
        unsafe {
            Map {
                game,
                lower: (read(world + range.lower_x), read(world + range.lower_y)),
                upper: (read(world + range.upper_x), read(world + range.upper_y)),
                explored: read(world + bindings.explored),
            }
        }
    }

    pub fn id(&self) -> MapId {
        MapId {
            explored: self.explored,
            lower: self.lower,
            upper: self.upper,
        }
    }

    /// How lit the map is at `pos`, blended between the hexes around it. Biomes light their hexes
    /// (0.5 by default, from 0.3 to 1 in this game version), and dark areas, which the player lights up
    /// with light cells, set theirs to 0. `None` if this version of the game doesn't have
    /// `light_value` as expected.
    pub fn light_at(&self, pos: Real2) -> Option<f32> {
        let bindings = self.game.bindings;
        let light_value = bindings.light_value.ok()?;
        // SAFETY: The game's `light_value` with its `w.map`, which only reads the map.
        let light = unsafe {
            let light_value: LightValue = std::mem::transmute(light_value);
            light_value((bindings.world + bindings.map) as *const c_void, pos)
        };
        light.is_finite().then_some(light)
    }

    /// Samples the wall distance field the game's physics uses, at `pos`.
    pub fn wall_at(&self, pos: Real2) -> Wall {
        let bindings = self.game.bindings;
        // SAFETY: The game's `wall_map` with its `w.map`; `true` is what the pickup physics passes.
        unsafe {
            let wall_map: WallMap = std::mem::transmute(bindings.wall_map);
            let sample = wall_map((bindings.world + bindings.map) as *const c_void, pos, true);
            Wall::from(sample)
        }
    }
}
