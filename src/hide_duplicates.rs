//! Hides the game's own version of what the map shows, so it only shows once. While the map icons
//! are spread out onto a grid, the pickups on it aren't drawn in the world (if they're near enough to
//! be). While the map shows tooltips, the game's tooltip for the pickup under the mouse in the world
//! isn't shown.

use modkit::Feature;
use modkit::game::{Frame, Game, PickupsId, Stage};
use modkit::log;

use crate::grid_pickups::GridPickups;
use crate::map_icons::SHOW_TOOLTIPS;

pub struct HideDuplicates {
    /// The pickups whose map icons are on the grid, shared by the map icons.
    grid: GridPickups,
    hidden: Hidden,
    /// Whether the game's tooltip for the pickup under the mouse in the world was active, and its
    /// opacity, while it's hidden.
    hidden_world_tooltip: Option<(bool, f32)>,
    logged_unrestored: bool,
}

/// Pickups on the grid hidden in the world, to be shown again.
#[derive(Default)]
struct Hidden {
    /// The pickup array they were hidden in.
    pickups: Option<PickupsId>,
    /// Index of each hidden pickup, and its opacity.
    alphas: Vec<(usize, f32)>,
}

impl Feature for HideDuplicates {
    fn name(&self) -> &'static str {
        "hiding duplicates"
    }

    fn stage_begin(&mut self, frame: &Frame, stage: Stage) {
        if stage == Stage::CELL_PICKUPS {
            self.hide_in_world(frame.game());
        } else if stage == Stage::RACING_OVERLAY {
            self.hide_world_tooltip(frame.game());
        }
    }

    fn stage_end(&mut self, frame: &Frame, stage: Stage) {
        if stage == Stage::CELL_PICKUPS {
            self.restore_in_world(frame.game());
        } else if stage == Stage::RACING_OVERLAY {
            self.show_world_tooltip(frame.game());
        }
    }

    fn revert(&mut self, game: &Game) {
        // No logging here: it may run while the game's threads are paused.
        self.show_in_world(game);
        self.show_world_tooltip(game);
    }
}

impl HideDuplicates {
    pub fn new(grid: GridPickups) -> Self {
        HideDuplicates {
            grid,
            hidden: Hidden::default(),
            hidden_world_tooltip: None,
            logged_unrestored: false,
        }
    }

    /// Makes the pickups on the grid transparent while the game queues the pickups near the camera
    /// to be drawn in the world, until `show_in_world`: their icons stand in for them. Not once the map
    /// starts closing: its icons fade out then, and the pickups should be back at once.
    fn hide_in_world(&mut self, game: &Game) {
        self.restore_in_world(game);
        if !game.map_mode() {
            return;
        }
        let pickups = game.pickups();
        let hidden = &mut self.hidden;
        hidden.pickups = Some(pickups.id());
        self.grid.read(pickups.id(), |on_grid| {
            for &index in on_grid {
                if let Some(pickup) = pickups.get(index) {
                    hidden.alphas.push((index, pickup.alpha()));
                    pickup.set_alpha(0.0);
                }
            }
        });
    }

    /// Shows the pickups hidden in the world again, logging (once) if the pickups changed in between.
    fn restore_in_world(&mut self, game: &Game) {
        if !self.show_in_world(game) && !self.logged_unrestored {
            self.logged_unrestored = true;
            log::warn("pickups changed while hidden in the world; showed those still hidden");
        }
    }

    /// Shows the pickups hidden in the world again. Returns false if the pickup array changed in
    /// between.
    fn show_in_world(&mut self, game: &Game) -> bool {
        if self.hidden.alphas.is_empty() {
            return true;
        }
        let pickups = game.pickups();
        // Nothing should change the pickups in between. If something did, an index may now refer to
        // another pickup, so only a pickup still hidden is shown again. Leaving them all instead would
        // leave them hidden for good.
        for &(index, alpha) in &self.hidden.alphas {
            if let Some(pickup) = pickups.get(index)
                && pickup.alpha().to_bits() == 0f32.to_bits()
            {
                pickup.set_alpha(alpha);
            }
        }
        self.hidden.alphas.clear();
        self.hidden.pickups == Some(pickups.id())
    }

    /// While the map is open and shows tooltips, keeps the game from drawing its tooltip for the
    /// pickup under the mouse in the world (it does in this stage) until `show_world_tooltip`: it
    /// would show up on the map, next to the one for the icon under the mouse.
    fn hide_world_tooltip(&mut self, game: &Game) {
        self.show_world_tooltip(game);
        if !game.map_open() || !SHOW_TOOLTIPS.get() {
            return;
        }
        let tooltip = game.world_tooltip();
        self.hidden_world_tooltip = Some((tooltip.active(), tooltip.alpha()));
        tooltip.set_active(false);
        tooltip.set_alpha(0.0);
    }

    fn show_world_tooltip(&mut self, game: &Game) {
        let Some((active, alpha)) = self.hidden_world_tooltip.take() else {
            return;
        };
        let tooltip = game.world_tooltip();
        // Nothing should change it in between, but if something did, leave it be.
        if !tooltip.active() && tooltip.alpha().to_bits() == 0f32.to_bits() {
            tooltip.set_active(active);
            tooltip.set_alpha(alpha);
        }
    }
}
