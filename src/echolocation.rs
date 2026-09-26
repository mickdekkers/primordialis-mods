//! Shows the Echolocation mutation's pickup markers where pickups will settle, like the map icons,
//! instead of inside rock, and hides the markers of pickups whose map icons are spread out on a grid:
//! pickups are moved while the markers are drawn, and moved back right after.

use modkit::Feature;
use modkit::game::{Frame, Game, PickupsId, Real2, Stage};
use modkit::log;
use modkit::settings::{Setting, Toggle};

use crate::grid_pickups::GridPickups;
use crate::settle::SettledPositions;

static FIX_ECHOLOCATION_POSITIONS: Toggle = Toggle::new(
    "fix_echolocation_positions",
    true,
    "Also show the Echolocation mutation's pickup markers where pickups will end up, like the map icons.",
);

/// The game only draws markers for pickups within range of the camera. Pickups moved here are out
/// of range from anywhere.
const OUT_OF_RANGE: Real2 = Real2::new(f32::MAX, f32::MAX);

pub struct EcholocationFix {
    grid: GridPickups,
    settled: SettledPositions,
    moved: Moved,
    logged_unrestored: bool,
}

/// Pickups moved for the markers, to be moved back.
#[derive(Default)]
struct Moved {
    /// The pickup array they were moved in.
    pickups: Option<PickupsId>,
    /// Index of each moved pickup, its original position, and what we wrote.
    positions: Vec<(usize, Real2, Real2)>,
}

impl Feature for EcholocationFix {
    fn name(&self) -> &'static str {
        "Echolocation marker fix"
    }

    fn settings(&self) -> Vec<&'static dyn Setting> {
        vec![&FIX_ECHOLOCATION_POSITIONS]
    }

    fn stage_begin(&mut self, frame: &Frame, stage: Stage) {
        if stage == Stage::RACING_OVERLAY {
            self.move_pickups(frame.game());
        }
    }

    fn stage_end(&mut self, frame: &Frame, stage: Stage) {
        if stage == Stage::RACING_OVERLAY {
            self.restore_pickups(frame.game());
        }
    }

    fn revert(&mut self, game: &Game) {
        // No logging here: it may run while the game's threads are paused.
        self.put_back(game);
    }
}

impl EcholocationFix {
    pub fn new(grid: GridPickups) -> Self {
        EcholocationFix {
            grid,
            settled: SettledPositions::default(),
            moved: Moved::default(),
            logged_unrestored: false,
        }
    }

    /// Moves pickups that are inside walls to where they will settle, and pickups on the map icon
    /// grid out of range, until `restore_pickups`. Only the Echolocation markers are drawn in
    /// between, so they are the only thing that sees it.
    fn move_pickups(&mut self, game: &Game) {
        self.restore_pickups(game);
        self.settled.refresh(game);
        let (pickups, map) = (game.pickups(), game.map());
        let fix_positions = FIX_ECHOLOCATION_POSITIONS.get();
        self.moved.pickups = Some(pickups.id());
        let moved = &mut self.moved.positions;
        let settled = &mut self.settled;
        self.grid.read(pickups.id(), |on_grid| {
            for (index, pickup) in pickups.iter().enumerate() {
                let original = pickup.position();
                let to = if on_grid.binary_search(&index).is_ok() {
                    OUT_OF_RANGE
                } else if fix_positions {
                    settled.get(&map, &pickup)
                } else {
                    continue;
                };
                if !to.same_bits(original) {
                    pickup.set_position(to);
                    moved.push((index, original, to));
                }
            }
        });
    }

    /// Moves the pickups back, logging (once) if the pickups changed in between.
    fn restore_pickups(&mut self, game: &Game) {
        if !self.put_back(game) && !self.logged_unrestored {
            self.logged_unrestored = true;
            log::warn(
                "pickups changed while moved for Echolocation; moved back those still where they were \
                 moved to",
            );
        }
    }

    /// Moves the pickups back. Returns false if the pickup array changed in between.
    fn put_back(&mut self, game: &Game) -> bool {
        if self.moved.positions.is_empty() {
            return true;
        }
        let pickups = game.pickups();
        // Nothing should change the pickups in between. If something did, an index may now refer to
        // another pickup, so only a pickup still exactly where it was moved to is moved back. Leaving
        // them all instead would leave those moved out of range there for good.
        for &(index, original, written) in &self.moved.positions {
            if let Some(pickup) = pickups.get(index)
                && pickup.position().same_bits(written)
            {
                pickup.set_position(original);
            }
        }
        self.moved.positions.clear();
        self.moved.pickups == Some(pickups.id())
    }
}
