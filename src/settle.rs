//! Where pickups will settle. The game only simulates pickups near the camera: far away, a pickup
//! stays where map generation put it, which can be inside rock. Once simulated, the physics pushes it
//! out to the wall surface. This applies the same push-out, to show pickups where they will be.

use modkit::game::{Game, Map, Pickup, Real2};
use rustc_hash::FxHashMap;

/// The game's physics keeps pickups at least this fraction of their radius away from walls.
const WALL_CLEARANCE: f32 = 0.5;
/// The game pushes a pickup out of walls a step per tick; a few steps always get it clear.
const MAX_PUSH_OUT_STEPS: usize = 8;

/// Settled positions, cached: pickups away from the camera don't move, so this saves sampling walls
/// for them every frame.
#[derive(Default)]
pub struct SettledPositions {
    /// Pickup position and radius (as bits) to its settled position.
    cache: FxHashMap<[u32; 3], Real2>,
    map_open: bool,
}

impl SettledPositions {
    /// Call once per frame, before `get`.
    pub fn refresh(&mut self, game: &Game) {
        // Forget everything whenever the map opens, in case walls changed.
        let map_open = game.map_open();
        if map_open && !self.map_open {
            self.cache.clear();
        }
        self.map_open = map_open;
        // Pickups near the camera move every frame, adding new entries; don't let them pile up.
        if self.cache.len() > 4 * game.pickups().len() + 1024 {
            self.cache.clear();
        }
    }

    /// Where `pickup` will be once the game's physics pushes it out of walls.
    pub fn get(&mut self, map: &Map, pickup: &Pickup) -> Real2 {
        let (position, radius) = (pickup.position(), pickup.radius());
        let key = [position.x.to_bits(), position.y.to_bits(), radius.to_bits()];
        *self.cache.entry(key).or_insert_with(|| {
            let wall_at = |at| {
                let wall = map.wall_at(at);
                (wall.dist, wall.gradient)
            };
            push_out_of_walls(wall_at, position, radius)
        })
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
}
