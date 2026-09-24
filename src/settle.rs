//! Where pickups will settle. The game only simulates pickups near the camera: far away, a pickup
//! stays where map generation put it, which can be inside rock. Once simulated, the physics pushes it
//! out to the wall surface. This applies the same push-out, to show pickups where they will be.

use rustc_hash::FxHashMap;

use modkit::game::{Game, Map, Pickup, Real2};

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
        *self
            .cache
            .entry(key)
            .or_insert_with(|| push_out_of_walls(map, position, radius))
    }
}

/// Where the game's physics will move a pickup once it simulates it: out of any wall, along the wall
/// distance field's gradient, until it's at least `WALL_CLEARANCE` of its radius from the surface.
fn push_out_of_walls(map: &Map, spawned_at: Real2, radius: f32) -> Real2 {
    let clearance = WALL_CLEARANCE * radius;
    let mut pos = spawned_at;
    for _ in 0..MAX_PUSH_OUT_STEPS {
        let wall = map.wall_at(pos);
        let depth = clearance - wall.dist;
        if depth.is_nan() || depth <= 0.0 {
            break;
        }
        pos.x += wall.gradient.x * depth;
        pos.y += wall.gradient.y * depth;
    }
    if pos.is_finite() { pos } else { spawned_at }
}
