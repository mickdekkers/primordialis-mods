//! The world: cell pickups, their materials, and the map.

use std::ffi::c_void;

use super::types::{Real2, Wall, WallSample};
use super::{Game, read, write};

/// Map hexes are 200 units apart: hex (q, r) is centered at (200q + 100r, 173.205r).
const HEX_SPACING: f32 = 200.0;
const HEX_ROW_HEIGHT: f32 = 173.205_08;

/// `wall_t wall_map(map_t*, real_2, bool)`. The 24-byte `wall_t` is returned through a hidden pointer
/// argument, which Rust does as well for this signature.
type WallMap = unsafe extern "C" fn(*const c_void, Real2, bool) -> WallSample;

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

/// The world map (`w.map`): a grid of hexes, and the walls.
#[derive(Clone, Copy)]
pub struct Map<'a> {
    game: Game<'a>,
    /// Hex bounds of the `explored` grid: `lower` inclusive, `upper` exclusive.
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

    /// How explored the map hex containing `pos` is, as the map shows it: 0 unexplored, rising to 1
    /// as the player sees it.
    pub fn explored_at(&self, pos: Real2) -> f32 {
        let (q, r) = hex_at(pos);
        let (lower, upper) = (self.lower, self.upper);
        if self.explored == 0 || q < lower.0 || q >= upper.0 || r < lower.1 || r >= upper.1 {
            return 0.0;
        }
        let width = (upper.0 - lower.0) as usize;
        let index = (r - lower.1) as usize * width + (q - lower.0) as usize;
        // SAFETY: The game's `(upper_q - lower_q) * (upper_r - lower_r)` values, and the index is
        // within the bounds checked above.
        unsafe { read((self.explored as *const f32).add(index) as usize) }
    }

    /// Samples the wall distance field the game's physics uses, at `pos`.
    pub fn wall_at(&self, pos: Real2) -> Wall {
        let bindings = self.game.bindings;
        // SAFETY: The game's `wall_map` with its `w.map`; `true` is what the pickup physics passes.
        unsafe {
            let wall_map: WallMap = std::mem::transmute(bindings.wall_map);
            let sample = wall_map((bindings.world + bindings.map) as *const c_void, pos, true);
            Wall::new(sample, bindings.wall_extras.is_available())
        }
    }
}

/// The map hex containing a world position, in axial coordinates.
fn hex_at(pos: Real2) -> (i32, i32) {
    let r = pos.y / HEX_ROW_HEIGHT;
    let q = (pos.x - 0.5 * HEX_SPACING * r) / HEX_SPACING;
    // Round in cube coordinates (q + r + s = 0), fixing up whichever component rounded the most.
    let s = -q - r;
    let (mut rq, mut rr, rs) = (q.round(), r.round(), s.round());
    let (dq, dr, ds) = ((rq - q).abs(), (rr - r).abs(), (rs - s).abs());
    if dq > dr && dq > ds {
        rq = -rr - rs;
    } else if dr > ds {
        rr = -rq - rs;
    }
    (rq as i32, rr as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn center(q: i32, r: i32) -> Real2 {
        Real2 {
            x: HEX_SPACING * q as f32 + 0.5 * HEX_SPACING * r as f32,
            y: HEX_ROW_HEIGHT * r as f32,
        }
    }

    #[test]
    fn hex_centers_round_trip() {
        for q in -20..20 {
            for r in -20..20 {
                assert_eq!(hex_at(center(q, r)), (q, r));
            }
        }
    }

    #[test]
    fn points_near_a_center_belong_to_its_hex() {
        let c = center(3, -7);
        for (dx, dy) in [
            (90.0, 0.0),
            (-90.0, 0.0),
            (0.0, 90.0),
            (0.0, -90.0),
            (60.0, 60.0),
            (-60.0, -60.0),
        ] {
            assert_eq!(
                hex_at(Real2 {
                    x: c.x + dx,
                    y: c.y + dy
                }),
                (3, -7)
            );
        }
    }
}
