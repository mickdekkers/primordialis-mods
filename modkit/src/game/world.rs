//! The world: cell pickups, their materials, and the map.

use std::ffi::c_void;

use super::types::{Real2, Wall, WallSample};
use super::{Game, read, write};

/// `wall_t wall_map(map_t*, real_2, bool)`. The 24-byte `wall_t` is returned through a hidden pointer
/// argument, which Rust does as well for this signature.
type WallMap = unsafe extern "C" fn(*const c_void, Real2, bool) -> WallSample;
/// `float light_value(map_t*, real_2)`: blends `map.light` of the hexes around the position.
type LightValue = unsafe extern "C" fn(*const c_void, Real2) -> f32;

/// Combo pickups don't use their material's color: the game cycles them through a dim rainbow,
/// `COMBO_BASE + COMBO_AMPLITUDE * cos(frame_number * COMBO_SPEED + phase)` for red, green and blue,
/// with green and blue phase-shifted by 2/3 and 1/3 of a cycle. These are constants in
/// `render_game`'s code (not symbols), taken from the current build.
const COMBO_BASE: f32 = 0.35;
const COMBO_AMPLITUDE: f32 = 0.05;
/// Radians per `frame_number` tick. Angles from the frame number are worked out in `f64`: an `f32`
/// only holds tick counts exactly up to 2^24 (39 hours of play), after which they'd move in steps.
const COMBO_SPEED: f64 = 0.02;

/// The game's physics keeps pickups at least this fraction of their radius away from walls: a
/// constant in `update_cells`' code (not a symbol), taken from the current build, which pushes a
/// pickup out along the gradient while `0.5 * r > wall.dist`.
const WALL_CLEARANCE: f32 = 0.5;
/// The game pushes a pickup out of walls a step per tick; a few steps always get it clear. Our
/// choice, not the game's.
const MAX_PUSH_OUT_STEPS: usize = 8;

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

impl Game<'_> {
    /// The color the game gives combo pickups this frame (see `Pickup::is_combo`), instead of their
    /// material's: RGB, shifting through a dim rainbow.
    pub fn combo_color(&self) -> [f32; 3] {
        combo_color(self.frame_number())
    }
}

/// The color the game gives combo pickups on a given frame.
fn combo_color(frame_number: i32) -> [f32; 3] {
    use std::f64::consts::TAU;
    let t = f64::from(frame_number) * COMBO_SPEED;
    [0.0, 2.0 / 3.0, 1.0 / 3.0]
        .map(|phase| COMBO_BASE + COMBO_AMPLITUDE * ((t + phase * TAU) % TAU).cos() as f32)
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

    /// The light (`light_at`) most biomes give their hexes, and so most of the map: the default of
    /// `biome_type.light`, a value in the game's biome definitions (not a symbol), taken from the
    /// current build.
    pub const NORMAL_LIGHT: f32 = 0.5;

    pub fn id(&self) -> MapId {
        MapId {
            explored: self.explored,
            lower: self.lower,
            upper: self.upper,
        }
    }

    /// How lit the map is at `pos`, blended between the hexes around it. Biomes light their hexes
    /// (`NORMAL_LIGHT` by default, from 0.3 to 1 in this game version), and dark areas, which the
    /// player lights up with light cells, set theirs to 0. `None` if this version of the game doesn't
    /// have `light_value` as expected.
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

    /// Where `pickup` will be once the game's physics pushes it out of walls. The game only simulates
    /// pickups near the camera: far away, a pickup stays where map generation put it, which can be
    /// inside rock, until the physics pushes it out to the wall surface as the camera comes near.
    pub fn settled_position(&self, pickup: &Pickup) -> Real2 {
        let wall_at = |at| {
            let wall = self.wall_at(at);
            (wall.dist, wall.gradient)
        };
        push_out_of_walls(wall_at, pickup.position(), pickup.radius())
    }

    /// Samples the wall distance field the game's physics uses, at `pos`.
    fn wall_at(&self, pos: Real2) -> Wall {
        let bindings = self.game.bindings;
        // SAFETY: The game's `wall_map` with its `w.map`; `true` is what the pickup physics passes.
        unsafe {
            let wall_map: WallMap = std::mem::transmute(bindings.wall_map);
            let sample = wall_map((bindings.world + bindings.map) as *const c_void, pos, true);
            Wall::from(sample)
        }
    }
}

/// Where the game's physics will move a pickup once it simulates it: out of any wall, along the wall
/// distance field's gradient, until it's at least `WALL_CLEARANCE` of its radius from the surface.
/// `wall_at` samples the field: the distance to the nearest wall surface (negative inside a wall),
/// and the unit direction away from it.
fn push_out_of_walls(
    wall_at: impl Fn(Real2) -> (f32, Real2),
    spawned_at: Real2,
    radius: f32,
) -> Real2 {
    let clearance = WALL_CLEARANCE * radius;
    let mut pos = spawned_at;
    for _ in 0..MAX_PUSH_OUT_STEPS {
        let (dist, gradient) = wall_at(pos);
        let depth = clearance - dist;
        if depth.is_nan() || depth <= 0.0 {
            break;
        }
        pos.x += gradient.x * depth;
        pos.y += gradient.y * depth;
    }
    if pos.is_finite() { pos } else { spawned_at }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rock below y = 0.
    fn floor(at: Real2) -> (f32, Real2) {
        (at.y, Real2::new(0.0, 1.0))
    }

    /// A round rock of radius 10 around the origin.
    fn boulder(at: Real2) -> (f32, Real2) {
        let d = at.distance(Real2::default());
        (d - 10.0, Real2::new(at.x / d, at.y / d))
    }

    #[test]
    fn pickups_in_rock_move_just_clear_of_it() {
        let settled = push_out_of_walls(floor, Real2::new(3.0, -5.0), 2.0);
        assert_eq!(settled, Real2::new(3.0, WALL_CLEARANCE * 2.0));
        let settled = push_out_of_walls(boulder, Real2::new(0.0, 4.0), 2.0);
        assert!((settled.y - 11.0).abs() < 1e-4 && settled.x == 0.0);
        // Diagonally, straight away from the boulder's center.
        let settled = push_out_of_walls(boulder, Real2::new(-3.0, 4.0), 2.0);
        assert!((settled.x + 6.6).abs() < 1e-4 && (settled.y - 8.8).abs() < 1e-4);
    }

    #[test]
    fn pickups_clear_of_rock_stay() {
        let at = Real2::new(3.0, 50.0);
        assert!(push_out_of_walls(floor, at, 2.0).same_bits(at));
    }

    #[test]
    fn broken_samples_leave_pickups_where_they_are() {
        let at = Real2::new(3.0, -5.0);
        let nan = |_| (f32::NAN, Real2::new(0.0, 1.0));
        assert!(push_out_of_walls(nan, at, 2.0).same_bits(at));
        let infinite = |_| (-1.0, Real2::new(f32::INFINITY, 0.0));
        assert!(push_out_of_walls(infinite, at, 2.0).same_bits(at));
    }

    #[test]
    fn combo_colors_cycle_within_the_games_range() {
        let period = std::f64::consts::TAU / COMBO_SPEED;
        for frame_number in (0..2000).step_by(7) {
            let color = combo_color(frame_number);
            for channel in color {
                assert!(
                    (COMBO_BASE - COMBO_AMPLITUDE - 1e-6..=COMBO_BASE + COMBO_AMPLITUDE + 1e-6)
                        .contains(&channel)
                );
            }
        }
        let red = combo_color(0)[0];
        assert!((red - (COMBO_BASE + COMBO_AMPLITUDE)).abs() < 1e-6);
        let half_cycle = combo_color((period / 2.0).round() as i32)[0];
        assert!((half_cycle - (COMBO_BASE - COMBO_AMPLITUDE)).abs() < 1e-4);
        // Green and blue peak a third and two thirds of a cycle after red.
        let green = combo_color((period / 3.0).round() as i32)[1];
        assert!((green - (COMBO_BASE + COMBO_AMPLITUDE)).abs() < 1e-4);
        let blue = combo_color((period * 2.0 / 3.0).round() as i32)[2];
        assert!((blue - (COMBO_BASE + COMBO_AMPLITUDE)).abs() < 1e-4);
    }

    #[test]
    fn combo_colors_keep_changing_every_tick_late_in_the_game() {
        for frame_number in [1_000, 100_000_000, i32::MAX - 1] {
            let (now, next) = (combo_color(frame_number), combo_color(frame_number + 1));
            assert!(now != next, "{frame_number}");
        }
    }
}
