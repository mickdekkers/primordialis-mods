//! The cells the player has found: the map only shows those. The player finds a cell by coming close
//! enough to see it, which depends on how lit its spot is while the player is near: as far as the fog
//! of war clears where it's lit, and closer the darker it is, so the dark areas meant to be searched
//! with a light cell keep their secrets. Found cells stay found, also in later game sessions (see
//! `store`), and only the player coming near finds one: a spot lit up later (glowing walls, say)
//! reveals nothing by itself.

mod store;

use std::time::{Duration, Instant};

use modkit::game::{Game, Map, MapId, Pickup, Pickups, PickupsId, Real2, SaveSlot};
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
/// A found pickup that moves further than this is saved at its new position: well within
/// `MATCH_DISTANCE`, so that it's still matched from the file, but far enough that pickups drifting
/// around near the player don't have the file written every `SAVE_INTERVAL`.
const RESAVE_DISTANCE: f32 = MATCH_DISTANCE / 5.0;

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

/// A pickup a found cell may belong to, while matching them up: what matching needs to know about
/// it, as plain data.
#[derive(Clone, Copy, Debug)]
struct Candidate {
    index: usize,
    material: u32,
    position: Real2,
}

impl Candidate {
    /// The pickup at `index`, if it gives a cell.
    fn of(index: usize, pickup: &Pickup) -> Option<Self> {
        Some(Candidate {
            index,
            material: pickup.material()?.id(),
            position: pickup.position(),
        })
    }
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
    /// Whether one of the game's menus was open last frame: the cells are saved as soon as one opens,
    /// since quitting the game goes through a menu, and nothing saves them on the way out.
    menu_open: bool,
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
        let candidate = |index| Candidate::of(index, &pickups.get(index)?);
        if self.pickups != Some(pickups.id()) || !self.follow(candidate) {
            let candidates = pickups
                .iter()
                .enumerate()
                .filter_map(|(index, pickup)| Candidate::of(index, &pickup));
            self.rematch(pickups.id(), pickups.len(), candidates);
        }
        let loaded_long_ago = self
            .loaded
            .is_some_and(|loaded| loaded.elapsed() >= PENDING_TIMEOUT);
        if !self.pending.is_empty() && loaded_long_ago && !pickups.is_empty() {
            self.pending.clear();
            self.dirty = true;
        }
        self.find(game, &map, &pickups);
        let menu_open = game.menu_open();
        if menu_open && !self.menu_open {
            self.save();
        } else {
            self.save_if_due();
        }
        self.menu_open = menu_open;
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

    /// Follows the found pickups, which only move a little from frame to frame. `pickup` gives the
    /// pickup at an index, if there is one that gives a cell. Returns false if one isn't where it
    /// was: the pickups changed without the array changing.
    fn follow(&mut self, pickup: impl Fn(usize) -> Option<Candidate>) -> bool {
        for tracked in &mut self.tracked {
            let Some(pickup) = pickup(tracked.index) else {
                return false;
            };
            let position = pickup.position;
            let moved = position.distance(to_real2(tracked.cell.position));
            if pickup.material != tracked.cell.material || moved.is_nan() || moved > MAX_STEP {
                return false;
            }
            if moved > RESAVE_DISTANCE {
                tracked.cell.position = [position.x, position.y];
                self.dirty = true;
            }
        }
        true
    }

    /// Matches the found cells with the pickups, after the pickups changed: the array `pickups`, of
    /// `len` pickups, of which `candidates` give cells. Cells whose pickups disappeared were picked
    /// up (or merged), unless the whole world is being cleared.
    fn rematch(
        &mut self,
        pickups: PickupsId,
        len: usize,
        candidates: impl Iterator<Item = Candidate>,
    ) {
        let shrunk = self.pickups_len.saturating_sub(len);
        let clearing = len == 0 || shrunk > MAX_PICKED_AT_ONCE;
        let mut squares: FxHashMap<(i32, i32), Vec<Candidate>> = FxHashMap::default();
        for candidate in candidates {
            squares
                .entry(square_at(candidate.position))
                .or_default()
                .push(candidate);
        }
        // Each found cell, whether it was tracked, and when it was found (to keep fading in).
        let found_at = std::mem::take(&mut self.indices);
        let tracked = self.tracked.drain(..).map(|tracked| {
            let at = found_at.get(&tracked.index).copied().flatten();
            (tracked.cell, true, at)
        });
        let pending = self.pending.drain(..).map(|cell| (cell, false, None));
        let cells: Vec<(Cell, bool, Option<i32>)> = tracked.chain(pending).collect();
        for (cell, was_tracked, found_at) in cells {
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
        self.pickups = Some(pickups);
        self.pickups_len = len;
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

/// Saves what's unsaved when the mod is unloaded, as the hot reload host does to swap builds: the
/// features are dropped once the hooks are removed and the game's threads run again. When the game
/// exits nothing is dropped, which is why they're also saved as a menu opens.
impl Drop for FoundCells {
    fn drop(&mut self) {
        self.save();
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

    fn pickup(index: usize, material: u32, x: f32) -> Candidate {
        Candidate {
            index,
            material,
            position: Real2::new(x, 0.0),
        }
    }

    fn cell(material: u32, x: f32) -> Cell {
        Cell {
            material,
            position: [x, 0.0],
        }
    }

    /// Matches `found` with a new pickup array holding `pickups`.
    fn rematch(found: &mut FoundCells, address: usize, pickups: &[Candidate]) {
        let len = pickups.len();
        let id = PickupsId::for_tests(address, len);
        found.rematch(id, len, pickups.iter().copied());
    }

    /// Cells as the file gives them, matched with `pickups`.
    fn loaded(cells: &[Cell], pickups: &[Candidate]) -> FoundCells {
        let mut found = FoundCells::default();
        found.pending = cells.to_vec();
        rematch(&mut found, 0x1000, pickups);
        found
    }

    /// The indices of the pickups shown as found, out of the first `len`.
    fn shown(found: &FoundCells, len: usize) -> Vec<usize> {
        (0..len).filter(|&i| found.alpha(i, 0).is_some()).collect()
    }

    #[test]
    fn cells_from_the_file_match_the_nearest_pickup_of_their_kind() {
        let found = loaded(
            &[cell(1, 0.0), cell(1, 10.0), cell(2, 100.0)],
            &[
                pickup(0, 1, 12.0),
                pickup(1, 1, 1.0),
                pickup(2, 2, 140.0),
                pickup(3, 3, 100.0),
                pickup(4, 1, 500.0),
            ],
        );
        assert_eq!(shown(&found, 5), [0, 1, 2]);
        assert!(found.pending.is_empty());
        assert_eq!(found.alpha(1, 0), Some(1.0), "shown at once, not faded in");
        assert!(!found.dirty);
    }

    #[test]
    fn a_found_pickup_that_disappears_was_picked_up() {
        let mut found = loaded(
            &[cell(1, 0.0), cell(2, 100.0)],
            &[pickup(0, 1, 0.0), pickup(1, 2, 100.0), pickup(2, 3, 300.0)],
        );
        rematch(
            &mut found,
            0x1000,
            &[pickup(0, 2, 100.0), pickup(1, 3, 300.0)],
        );
        assert_eq!(shown(&found, 2), [0]);
        assert!(found.pending.is_empty(), "the picked up cell is forgotten");
        assert!(found.dirty);
    }

    #[test]
    fn cells_stay_found_while_the_world_is_cleared() {
        let pickups: Vec<Candidate> = (0..20).map(|i| pickup(i, 1, i as f32 * 100.0)).collect();
        let cells: Vec<Cell> = pickups.iter().map(|p| cell(1, p.position.x)).collect();
        let mut found = loaded(&cells, &pickups);
        assert_eq!(shown(&found, 20).len(), 20);

        rematch(&mut found, 0x2000, &[]);
        assert_eq!(found.pending.len(), 20, "no pickups: the world is cleared");
        rematch(&mut found, 0x1000, &pickups);
        assert_eq!(shown(&found, 20).len(), 20);

        rematch(&mut found, 0x1000, &pickups[..20 - MAX_PICKED_AT_ONCE - 1]);
        assert_eq!(
            found.pending.len(),
            MAX_PICKED_AT_ONCE + 1,
            "too many at once"
        );
        assert!(!found.dirty);
    }

    #[test]
    fn found_pickups_are_followed_as_they_move() {
        let mut found = loaded(&[cell(1, 0.0)], &[pickup(0, 1, 0.0)]);
        let at = |material: u32, x: f32| move |index| (index == 0).then(|| pickup(0, material, x));
        assert!(found.follow(at(1, RESAVE_DISTANCE / 2.0)));
        assert_eq!(found.tracked[0].cell.position, [0.0, 0.0]);
        assert!(!found.dirty, "moving a little doesn't need saving");
        assert!(found.follow(at(1, 30.0)));
        assert_eq!(found.tracked[0].cell.position, [30.0, 0.0]);
        assert!(found.dirty);

        assert!(
            !found.follow(at(1, 30.0 + MAX_STEP + 1.0)),
            "too far in a frame"
        );
        assert!(!found.follow(at(2, 30.0)), "another kind of cell");
        assert!(!found.follow(|_| None), "gone");
    }

    #[test]
    fn a_found_pickup_keeps_fading_in_after_the_pickups_change() {
        let mut found = loaded(&[cell(1, 0.0)], &[pickup(0, 1, 0.0)]);
        // Found this session, at frame 100.
        found.indices.insert(0, Some(100));
        rematch(&mut found, 0x1000, &[pickup(0, 1, 0.0), pickup(1, 2, 60.0)]);
        let alpha = found.alpha(0, 130).unwrap();
        assert!(alpha > 0.0 && alpha < 1.0, "{alpha}");
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
