//! Shows the Echolocation mutation's pickup markers where pickups will settle, like the map icons,
//! instead of inside rock: pickups are moved there while the markers are drawn, and moved back right
//! after.

use modkit::Feature;
use modkit::game::{Frame, Game, PickupsId, Real2, Stage};
use modkit::log;
use modkit::settings::{Setting, Toggle};

use crate::settle::SettledPositions;

static FIX_ECHOLOCATION_POSITIONS: Toggle = Toggle::new(
    "fix_echolocation_positions",
    true,
    "Also show the Echolocation mutation's pickup markers where pickups will end up, like the map icons.",
);

#[derive(Default)]
pub struct EcholocationFix {
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
        if stage == Stage::RACING_OVERLAY && FIX_ECHOLOCATION_POSITIONS.get() {
            self.move_pickups(frame.game());
        }
    }

    fn stage_end(&mut self, frame: &Frame, stage: Stage) {
        if stage == Stage::RACING_OVERLAY {
            self.restore_pickups(frame.game());
        }
    }

    fn revert(&mut self, game: &Game) {
        self.restore_pickups(game);
    }
}

impl EcholocationFix {
    /// Moves pickups that are inside walls to where they will settle, until `restore_pickups`. Only
    /// the Echolocation markers are drawn in between, so they are the only thing that sees it.
    fn move_pickups(&mut self, game: &Game) {
        self.restore_pickups(game);
        self.settled.refresh(game);
        let (pickups, map) = (game.pickups(), game.map());
        self.moved.pickups = Some(pickups.id());
        for (index, pickup) in pickups.iter().enumerate() {
            let original = pickup.position();
            let settled = self.settled.get(&map, &pickup);
            if !settled.same_bits(original) {
                pickup.set_position(settled);
                self.moved.positions.push((index, original, settled));
            }
        }
    }

    fn restore_pickups(&mut self, game: &Game) {
        if self.moved.positions.is_empty() {
            return;
        }
        let pickups = game.pickups();
        // Nothing should change the pickups in between, but if something did, leave them be: a moved
        // pickup is only where the physics would have put it anyway.
        if self.moved.pickups == Some(pickups.id()) {
            for &(index, original, written) in &self.moved.positions {
                let Some(pickup) = pickups.get(index) else {
                    continue;
                };
                if pickup.position().same_bits(written) {
                    pickup.set_position(original);
                }
            }
        } else if !self.logged_unrestored {
            self.logged_unrestored = true;
            log::warn(
                "pickups changed while moved for Echolocation; left them at the moved positions",
            );
        }
        self.moved.positions.clear();
    }
}
