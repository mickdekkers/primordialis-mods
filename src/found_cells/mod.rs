//! The cells the player has found: the map only shows those. The player finds a cell by coming close
//! enough to see it, which depends on how lit its spot is while the player is near: as far as the fog
//! of war clears where it's lit, and closer the darker it is, so the dark areas meant to be searched
//! with a light cell keep their secrets. Found cells stay found, also in later game sessions (see
//! `store`), and only the player coming near finds one: a spot lit up later (glowing walls, say)
//! reveals nothing by itself.

mod store;

use std::time::{Duration, Instant};

use modkit::game::{Game, Map, MapId, Pickups, PickupsId, Real2, SaveSlot};
use modkit::log;
use rustc_hash::FxHashMap;

use crate::math;
use store::{Cell, Run, Slot, Store};

/// The light (`Map::light_at`) of most of the map. Dark areas are 0 until something lights them up.
const NORMAL_LIGHT: f32 = 0.5;
/// How close the player has to get to a cell in total darkness to find it, in world units. Our
/// choice, tuned by eye.
const DARK_REACH: f32 = 400.0;

/// A newly found cell's icon fades in over this many `frame_number` ticks (120 per second): half a
/// second. Our choice, tuned by eye.
const FADE_IN_TICKS: f32 = 60.0;

/// How far a pickup may move in one frame and still be taken for the same one.
const MAX_STEP: f32 = 50.0;
/// How far a found cell may lie from a pickup of the same kind and still be taken for it, when the
/// pickups change or come from the file. Pickups are indexed in squares this wide.
const MATCH_DISTANCE: f32 = 50.0;
/// When more pickups than this disappear at once, the game is clearing the world (quitting to the
/// menu, say) rather than the player picking cells up: their cells stay found.
const MAX_PICKED_AT_ONCE: usize = 16;
/// Found cells that match no pickup are forgotten after the run has been loaded for this long: their
/// pickups were picked up in a session that ended before it saved.
const PENDING_TIMEOUT: Duration = Duration::from_secs(60);
/// How often the found cells are saved, at most, while they change.
const SAVE_INTERVAL: Duration = Duration::from_secs(5);
/// A found pickup that moves further than this is saved at its new position.
const RESAVE_DISTANCE: f32 = 1.0;

/// How close the player has to get to a cell to find it, where the light is `light` and the player
/// sees `vision` far (`Game::vision_radius`): that far at normal light and above, closing in to
/// `DARK_REACH` in total darkness.
fn reach(light: f32, vision: f32) -> f32 {
    let dark = DARK_REACH.min(vision);
    dark + (vision - dark) * (light / NORMAL_LIGHT).clamp(0.0, 1.0)
}

/// A pickup whose cell was found, by its index in the current pickup array.
struct Tracked {
    index: usize,
    cell: Cell,
}

/// A pickup a found cell may belong to, while matching them up.
struct Candidate {
    index: usize,
    material: u32,
    position: Real2,
}

/// The cells the player has found in the current run.
#[derive(Default)]
pub struct FoundCells {
    /// The run and map the cells were found in. Without a run (no save in use, e.g. before one is
    /// started), they're found but not saved.
    world: Option<(Option<Run>, MapId)>,
    store: Store,
    tracked: Vec<Tracked>,
    /// The pickup indices in `tracked`, with the `frame_number` each was found at in this game
    /// session, to fade its icon in. Cells found in an earlier session (from the file) show at once.
    indices: FxHashMap<usize, Option<i32>>,
    /// Found cells no pickup matched yet: from the file, or whose pickups disappeared while the game
    /// was clearing the world.
    pending: Vec<Cell>,
    /// The pickup array `tracked` refers to, and its length.
    pickups: Option<PickupsId>,
    pickups_len: usize,
    loaded: Option<Instant>,
    /// Whether the cells changed since they were last saved, and when that was.
    dirty: bool,
    saved: Option<Instant>,
}

impl FoundCells {
    /// Finds the cells the player is close to, and keeps up with the pickups. Call it every frame,
    /// with the map screen open or not: the player can move with it open.
    pub fn update(&mut self, game: &Game) {
        let (map, pickups) = (game.map(), game.pickups());
        let run = game.save_slot().map(|slot| Run {
            slot: match slot {
                SaveSlot::Normal => Slot::Normal,
                SaveSlot::Sandbox => Slot::Sandbox,
            },
            seed: game.seed(),
            started: game.run_started_at().to_bits(),
        });
        let world = (run, map.id());
        if self.world != Some(world) {
            self.switch_world(world);
        }
        if self.pickups != Some(pickups.id()) || !self.follow(&pickups) {
            self.rematch(&pickups);
        }
        let loaded_long_ago = self
            .loaded
            .is_some_and(|loaded| loaded.elapsed() >= PENDING_TIMEOUT);
        if !self.pending.is_empty() && loaded_long_ago && !pickups.is_empty() {
            self.pending.clear();
            self.dirty = true;
        }
        self.find(game, &map, &pickups);
        self.save_if_due();
    }

    /// How visible the icon of the pickup at `index` is at `frame_number`: not at all until the player
    /// finds it, then fading in.
    pub fn alpha(&self, index: usize, frame_number: i32) -> Option<f32> {
        let found_at = *self.indices.get(&index)?;
        Some(found_at.map_or(1.0, |found_at| fade_in(frame_number.wrapping_sub(found_at))))
    }

    /// Saves the last run's cells, and loads the new one's.
    fn switch_world(&mut self, world: (Option<Run>, MapId)) {
        self.save();
        self.pending = world.0.map(|run| self.store.cells(run)).unwrap_or_default();
        if !self.pending.is_empty() {
            log::info(&format!(
                "remembered {} found cells for this run",
                self.pending.len()
            ));
        }
        self.world = Some(world);
        self.tracked.clear();
        self.indices.clear();
        self.pickups = None;
        self.pickups_len = 0;
        self.loaded = Some(Instant::now());
    }

    /// Follows the found pickups, which only move a little from frame to frame. Returns false if one
    /// isn't where it was: the pickups changed without the array changing.
    fn follow(&mut self, pickups: &Pickups) -> bool {
        for tracked in &mut self.tracked {
            let Some(pickup) = pickups.get(tracked.index) else {
                return false;
            };
            let position = pickup.position();
            let moved = position.distance(to_real2(tracked.cell.position));
            if pickup.material().map(|material| material.id()) != Some(tracked.cell.material)
                || moved.is_nan()
                || moved > MAX_STEP
            {
                return false;
            }
            if moved > RESAVE_DISTANCE {
                tracked.cell.position = [position.x, position.y];
                self.dirty = true;
            }
        }
        true
    }

    /// Matches the found cells with the pickups, after the pickups changed. Cells whose pickups
    /// disappeared were picked up (or merged), unless the whole world is being cleared.
    fn rematch(&mut self, pickups: &Pickups) {
        let shrunk = self.pickups_len.saturating_sub(pickups.len());
        let clearing = pickups.is_empty() || shrunk > MAX_PICKED_AT_ONCE;
        let mut squares: FxHashMap<(i32, i32), Vec<Candidate>> = FxHashMap::default();
        for (index, pickup) in pickups.iter().enumerate() {
            if let Some(material) = pickup.material() {
                let position = pickup.position();
                squares
                    .entry(square_at(position))
                    .or_default()
                    .push(Candidate {
                        index,
                        material: material.id(),
                        position,
                    });
            }
        }
        // Each found cell, whether it was tracked, and when it was found (to keep fading in).
        let found_at = std::mem::take(&mut self.indices);
        let tracked = self.tracked.drain(..).map(|tracked| {
            let at = found_at.get(&tracked.index).copied().flatten();
            (tracked.cell, true, at)
        });
        let pending = self.pending.drain(..).map(|cell| (cell, false, None));
        let candidates: Vec<(Cell, bool, Option<i32>)> = tracked.chain(pending).collect();
        for (cell, was_tracked, found_at) in candidates {
            let at = to_real2(cell.position);
            let (x, y) = square_at(at);
            let nearest = (-1..=1)
                .flat_map(|dy| (-1..=1).map(move |dx| (x + dx, y + dy)))
                .filter_map(|square| squares.get(&square))
                .flatten()
                .filter(|candidate| {
                    candidate.material == cell.material
                        && !self.indices.contains_key(&candidate.index)
                        && candidate.position.distance(at) <= MATCH_DISTANCE
                })
                .min_by(|a, b| a.position.distance(at).total_cmp(&b.position.distance(at)));
            match nearest {
                Some(candidate) => {
                    self.indices.insert(candidate.index, found_at);
                    self.tracked.push(Tracked {
                        index: candidate.index,
                        cell: Cell {
                            material: candidate.material,
                            position: [candidate.position.x, candidate.position.y],
                        },
                    });
                }
                None if was_tracked && !clearing => self.dirty = true,
                None => self.pending.push(cell),
            }
        }
        self.pickups = Some(pickups.id());
        self.pickups_len = pickups.len();
    }

    /// Finds the cells the player is close enough to.
    fn find(&mut self, game: &Game, map: &Map, pickups: &Pickups) {
        let center = game.view_center();
        let vision = game.vision_radius();
        if !center.is_finite() || !vision.is_finite() {
            return;
        }
        let frame_number = game.frame_number();
        for (index, pickup) in pickups.iter().enumerate() {
            if self.indices.contains_key(&index) {
                continue;
            }
            let position = pickup.position();
            let distance = position.distance(center);
            // The light only matters within sight. If this version of the game can't tell, it counts
            // as normal.
            let near = distance <= vision
                && distance <= reach(map.light_at(position).unwrap_or(NORMAL_LIGHT), vision);
            if let Some(material) = pickup.material().filter(|_| near) {
                self.indices.insert(index, Some(frame_number));
                self.tracked.push(Tracked {
                    index,
                    cell: Cell {
                        material: material.id(),
                        position: [position.x, position.y],
                    },
                });
                self.dirty = true;
            }
        }
    }

    fn save_if_due(&mut self) {
        let due = self
            .saved
            .is_none_or(|saved| saved.elapsed() >= SAVE_INTERVAL);
        if self.dirty && due {
            self.save();
        }
    }

    /// Saves the current run's found cells, if they changed.
    fn save(&mut self) {
        let Some((Some(run), _)) = self.world else {
            return;
        };
        if !self.dirty {
            return;
        }
        let cells = self
            .tracked
            .iter()
            .map(|tracked| tracked.cell)
            .chain(self.pending.iter().copied())
            .collect();
        self.store.set(run, cells);
        // Tried again after the interval if it failed.
        self.dirty = !self.store.save();
        self.saved = Some(Instant::now());
    }
}

/// How far a found cell's icon has faded in, `ticks` after it was found. A frame number that went
/// back (it's the game's, not ours) counts as long ago, so no icon stays hidden.
fn fade_in(ticks: i32) -> f32 {
    if ticks < 0 {
        return 1.0;
    }
    math::smoothstep(0.0, FADE_IN_TICKS, ticks as f32)
}

fn to_real2([x, y]: [f32; 2]) -> Real2 {
    Real2::new(x, y)
}

fn square_at(position: Real2) -> (i32, i32) {
    (
        (position.x / MATCH_DISTANCE).floor() as i32,
        (position.y / MATCH_DISTANCE).floor() as i32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reach_grows_with_light_up_to_the_vision_radius() {
        let vision = 1000.0;
        assert_eq!(reach(0.0, vision), DARK_REACH);
        assert!(reach(0.3, vision) > reach(0.1, vision) && reach(0.3, vision) < vision);
        assert_eq!(reach(NORMAL_LIGHT, vision), vision);
        assert_eq!(reach(1.0, vision), vision);
        // Never further than the player sees, even in the dark.
        assert_eq!(reach(0.0, 300.0), 300.0);
    }

    #[test]
    fn found_cells_fade_in() {
        assert_eq!(fade_in(0), 0.0);
        assert!(fade_in(30) > 0.0 && fade_in(30) < 1.0);
        assert_eq!(fade_in(FADE_IN_TICKS as i32), 1.0);
        assert_eq!(fade_in(i32::MAX), 1.0);
        assert_eq!(fade_in(-5), 1.0, "the frame number went back");
    }

    #[test]
    fn neighboring_squares_cover_the_match_distance() {
        // A cell and a pickup within MATCH_DISTANCE are at most one square apart on each axis.
        let at = Real2::new(49.9, -0.1);
        let (x, y) = square_at(at);
        for (dx, dy) in [(1.0, 1.0), (-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0)] {
            let other = Real2::new(at.x + dx * 35.0, at.y + dy * 35.0);
            let (ox, oy) = square_at(other);
            assert!((ox - x).abs() <= 1 && (oy - y).abs() <= 1);
        }
    }
}
