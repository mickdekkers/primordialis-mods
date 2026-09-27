//! Where pickups will settle (`Map::settled_position`), cached: the game only simulates pickups near
//! the camera, and far away they stay where map generation put them, which can be inside rock.

use modkit::game::{Game, Map, Pickup, Real2};
use rustc_hash::FxHashMap;

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
            .or_insert_with(|| map.settled_position(pickup))
    }
}
